use super::{AgentResult as Result, Command, Error, Reply};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{
    env, fs, io,
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::PathBuf,
    ptr::null_mut,
    sync::mpsc,
    thread,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions},
    sync::oneshot,
    time::{sleep, timeout},
};
use windows_sys::Win32::{
    Foundation::{ERROR_PIPE_BUSY, LocalFree},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        },
        GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_GROUPS, TOKEN_QUERY, TokenLogonSid,
    },
    System::{
        Pipes::GetNamedPipeServerProcessId,
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

const DEADLINE: Duration = Duration::from_secs(2);
const REQUEST_LIMIT: usize = 2 * 1024 * 1024;
const RESPONSE_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct WindowInfo {
    window_id: String,
    pid: u32,
}

impl WindowInfo {
    fn pipe_name(&self) -> String {
        format!(r"\\.\pipe\DFlow-{}", self.window_id)
    }
}

pub struct Window {
    pub pending: mpsc::Receiver<(Command, oneshot::Sender<Result<Reply>>)>,
    path: PathBuf,
    _shutdown: oneshot::Sender<()>,
}

impl Window {
    pub fn start() -> io::Result<Self> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
        let record = WindowInfo {
            window_id: random.iter().map(|byte| format!("{byte:02x}")).collect(),
            pid: std::process::id(),
        };
        let directory = directory()?;
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!("{}.json", record.window_id));
        let (commands, pending) = mpsc::sync_channel(4);
        let (ready, status) = mpsc::sync_channel(1);
        let (shutdown, stop) = oneshot::channel();
        let pipe_name = record.pipe_name();
        thread::Builder::new()
            .name("dflow-agent".into())
            .spawn(move || {
                let runtime = match super::runtime() {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                runtime.block_on(async {
                    let server = create_pipe(&pipe_name);
                    match server {
                        Ok(mut server) => {
                            if ready.send(Ok(())).is_ok() {
                                tokio::select! {
                                    _ = accept(&mut server, commands) => {},
                                    _ = stop => {},
                                }
                            }
                        }
                        Err(error) => {
                            let _ = ready.send(Err(error));
                        }
                    }
                });
            })?;
        status.recv().map_err(io::Error::other)??;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(file.as_file_mut(), &record)?;
        file.persist_noclobber(&path).map_err(|error| error.error)?;
        Ok(Self {
            pending,
            path,
            _shutdown: shutdown,
        })
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn directory() -> io::Result<PathBuf> {
    env::var_os("LOCALAPPDATA")
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(path).join("DFlow").join("agent"))
        .ok_or_else(|| io::Error::other("LOCALAPPDATA is unavailable."))
}

async fn accept(
    server: &mut NamedPipeServer,
    commands: mpsc::SyncSender<(Command, oneshot::Sender<Result<Reply>>)>,
) {
    while server.connect().await.is_ok() {
        let _ = timeout(DEADLINE, async {
            let command = read_frame(server, REQUEST_LIMIT).await?;
            let (reply, receive) = oneshot::channel();
            let result: Result<Value> = if commands.try_send((command, reply)).is_err() {
                Err(Error::new("busy", "DFlow is busy."))
            } else {
                match receive.await {
                    Ok(Ok(reply)) => reply.finish().await,
                    Ok(Err(error)) => Err(error),
                    Err(_) => Err(Error::new("no_window", "The DFlow window closed.")),
                }
            };
            write_frame(server, &result, RESPONSE_LIMIT).await?;
            // DisconnectNamedPipe discards unread data. Let the client read and close first.
            let _ = server.read_u8().await;
            Ok::<(), io::Error>(())
        })
        .await;
        if server.disconnect().is_err() {
            break;
        }
    }
}

async fn read_frame<T: DeserializeOwned>(
    reader: &mut (impl AsyncReadExt + Unpin),
    limit: usize,
) -> io::Result<T> {
    let bytes = reader.read_u32_le().await? as usize;
    if bytes > limit {
        return Err(io::Error::other("DFlow message is too large."));
    }
    let mut data = vec![0; bytes];
    reader.read_exact(&mut data).await?;
    Ok(serde_json::from_slice(&data)?)
}

async fn write_frame<T: Serialize>(
    writer: &mut (impl AsyncWriteExt + Unpin),
    message: &T,
    limit: usize,
) -> io::Result<()> {
    let data = serde_json::to_vec(message)?;
    if data.len() > limit {
        return Err(io::Error::other("DFlow message is too large."));
    }
    writer.write_u32_le(data.len() as u32).await?;
    writer.write_all(&data).await
}

fn records() -> Result<Vec<WindowInfo>> {
    let path = directory().map_err(access_error)?;
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(access_error(error)),
    };
    let mut records: Vec<WindowInfo> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension()? != "json"
                || !entry.file_type().ok()?.is_file()
                || entry.metadata().ok()?.len() > 1024
            {
                return None;
            }
            let record: WindowInfo = serde_json::from_slice(&fs::read(&path).ok()?).ok()?;
            if record.window_id.len() != 32
                || !record
                    .window_id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
                || path.file_stem()? != record.window_id.as_str()
            {
                return None;
            }
            Some(record)
        })
        .collect();
    records.sort_by_key(|record| record.pid);
    Ok(records)
}

async fn connect(record: &WindowInfo) -> io::Result<NamedPipeClient> {
    for attempt in 0..=10 {
        let opened = ClientOptions::new().open(record.pipe_name());
        match opened {
            Ok(client) => {
                let mut pid = 0;
                // The handle remains valid for this query.
                if unsafe { GetNamedPipeServerProcessId(client.as_raw_handle(), &mut pid) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                if pid != record.pid {
                    return Err(io::Error::other(
                        "DFlow window no longer matches its discovery record.",
                    ));
                }
                return Ok(client);
            }
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) && attempt < 10 => {
                sleep(Duration::from_millis(25)).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("the last connection attempt returns")
}

pub(super) async fn windows() -> Result<Vec<WindowInfo>> {
    let mut windows = Vec::new();
    for record in records()? {
        match connect(&record).await {
            Ok(_) => windows.push(record),
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                windows.push(record);
            }
            Err(_) => {}
        }
    }
    Ok(windows)
}

pub async fn call(window_id: Option<&str>, command: Command) -> Result<Value> {
    let no_window = || {
        Error::new(
            "no_window",
            "No matching DFlow window is available. Open DFlow or run list_windows.",
        )
    };
    let record = if let Some(id) = window_id {
        records()?
            .into_iter()
            .find(|record| record.window_id == id)
            .ok_or_else(no_window)?
    } else {
        let mut windows = windows().await?;
        match windows.len() {
            0 => return Err(no_window()),
            1 => windows.remove(0),
            _ => {
                let mut error = Error::new(
                    "multiple_windows",
                    "Multiple DFlow windows are open. Pass the window_id of the intended window.",
                );
                error.windows = windows;
                return Err(error);
            }
        }
    };
    let mut client = match connect(&record).await {
        Ok(client) => client,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound
                || error.kind() == io::ErrorKind::PermissionDenied =>
        {
            return Err(no_window());
        }
        Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
            return Err(Error::new(
                "busy",
                "The DFlow window is busy. Try again shortly.",
            ));
        }
        Err(error) => return Err(access_error(error)),
    };
    let mut value = timeout(DEADLINE + Duration::from_millis(250), async {
        write_frame(&mut client, &command, REQUEST_LIMIT).await?;
        read_frame::<Result<Value>>(&mut client, RESPONSE_LIMIT).await
    })
    .await
    .map_err(|_| {
        Error::new(
            "timeout",
            "DFlow did not respond. Read the note before retrying an edit.",
        )
    })?
    .map_err(access_error)??;
    if let Some(object) = value.as_object_mut() {
        object.insert("window_id".into(), Value::String(record.window_id));
    }
    Ok(value)
}

fn access_error(error: io::Error) -> Error {
    Error::new(
        "connection_error",
        format!("Could not communicate with DFlow: {error}"),
    )
}

fn create_pipe(name: &str) -> io::Result<NamedPipeServer> {
    let sid = logon_sid()?;
    let sddl: Vec<u16> = format!("D:P(A;;GA;;;{sid})")
        .encode_utf16()
        .chain([0])
        .collect();
    let mut descriptor = null_mut();
    // The UTF-16 string is NUL-terminated and descriptor is an output pointer.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    // CreateNamedPipe copies the descriptor before returning.
    let pipe = unsafe {
        ServerOptions::new()
            .first_pipe_instance(true)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                name,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            )
    };
    unsafe { LocalFree(descriptor) };
    pipe
}

fn logon_sid() -> io::Result<String> {
    let mut handle = null_mut();
    // GetCurrentProcess supplies the process pseudo-handle. OpenProcessToken
    // produces an owned handle only on success.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut bytes = 0;
    // The size query intentionally passes no buffer.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenLogonSid,
            null_mut(),
            0,
            &mut bytes,
        )
    };
    if bytes < size_of::<TOKEN_GROUPS>() as u32 {
        return Err(io::Error::last_os_error());
    }
    let mut storage = vec![0usize; (bytes as usize).div_ceil(size_of::<usize>())];
    let groups = storage.as_mut_ptr().cast::<TOKEN_GROUPS>();
    // The allocation has sufficient size and alignment for TOKEN_GROUPS.
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenLogonSid,
            groups.cast(),
            bytes,
            &mut bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if unsafe { (*groups).GroupCount } != 1 {
        return Err(io::Error::other("Windows did not return one logon SID."));
    }
    let mut sid = null_mut();
    if unsafe { ConvertSidToStringSidW((*groups).Groups[0].Sid, &mut sid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // Windows returns a NUL-terminated string allocated with LocalAlloc.
    let mut len = 0;
    unsafe {
        while *sid.add(len) != 0 {
            len += 1;
        }
    }
    let result = String::from_utf16(unsafe { std::slice::from_raw_parts(sid, len) });
    unsafe { LocalFree(sid.cast()) };
    result.map_err(io::Error::other)
}
