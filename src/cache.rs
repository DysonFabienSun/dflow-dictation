use crate::editor::Editor;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    thread::{self, JoinHandle},
    time::Duration,
};

const RETRY_DELAY: Duration = Duration::from_millis(100);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(2);

#[derive(Default, Deserialize, Serialize)]
pub struct CachedNote {
    text: String,
    cursor: usize,
}

impl CachedNote {
    pub fn load(path: &Path) -> io::Result<Self> {
        let note: Self = match fs::read(path) {
            Ok(data) => serde_json::from_slice(&data)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        if note.text.chars().any(|c| c.is_control() && c != '\n') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid note text",
            ));
        }
        Ok(note)
    }

    pub fn into_editor(self) -> Editor {
        Editor::from_note(self.text, self.cursor)
    }

    fn save(&self, path: &Path, text_changed: bool) -> io::Result<()> {
        let directory = path
            .parent()
            .ok_or_else(|| io::Error::other("Missing cache directory"))?;
        fs::create_dir_all(directory)?;
        let lock = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("note.lock"))?;
        lock.lock()?;
        // Another window may have saved a different note since this window opened.
        if !text_changed && Self::load(path)?.text != self.text {
            return Ok(());
        }
        let mut temp = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(&mut temp, self)?;
        temp.as_file().sync_all()?;
        temp.persist(path).map_err(|error| error.error)?;
        Ok(())
    }
}

pub struct NoteCache {
    queue: SyncSender<(u64, CachedNote)>,
    feedback: Receiver<bool>,
    worker: JoinHandle<io::Result<()>>,
    queued: (u64, usize),
}

impl NoteCache {
    pub fn start(path: PathBuf, cursor: usize) -> io::Result<Self> {
        let (queue, pending) = mpsc::sync_channel::<(u64, CachedNote)>(1);
        let (report, feedback) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("dflow-cache".into())
            .spawn(move || {
                let mut saved_revision = 0;
                let mut failed_save: Option<(u64, CachedNote)> = None;
                let mut retry_delay = RETRY_DELAY;
                let mut last_report = None;
                loop {
                    let next = if failed_save.is_some() {
                        pending.recv_timeout(retry_delay)
                    } else {
                        pending.recv().map_err(|_| RecvTimeoutError::Disconnected)
                    };
                    let retrying = matches!(next, Err(RecvTimeoutError::Timeout));
                    let (revision, note) = match next {
                        Ok(snapshot) => snapshot,
                        Err(RecvTimeoutError::Timeout) => failed_save.take().unwrap(),
                        Err(RecvTimeoutError::Disconnected) => {
                            if let Some((revision, note)) = failed_save {
                                note.save(&path, revision != saved_revision)?;
                            }
                            return Ok(());
                        }
                    };
                    let saved = note.save(&path, revision != saved_revision).is_ok();
                    if saved {
                        saved_revision = revision;
                        failed_save = None;
                        retry_delay = RETRY_DELAY;
                    } else {
                        failed_save = Some((revision, note));
                        if retrying {
                            retry_delay = (retry_delay * 2).min(MAX_RETRY_DELAY);
                        }
                    }
                    if last_report != Some(saved) {
                        let _ = report.send(saved);
                        last_report = Some(saved);
                    }
                }
            })?;
        Ok(Self {
            queue,
            feedback,
            worker,
            queued: (0, cursor),
        })
    }

    pub fn update(&mut self, revision: u64, editor: &Editor) -> Option<bool> {
        let state = (revision, editor.cursor());
        if state != self.queued {
            let note = CachedNote {
                text: editor.text().to_owned(),
                cursor: state.1,
            };
            match self.queue.try_send((revision, note)) {
                Ok(()) => self.queued = state,
                Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => return Some(false),
            }
        }
        self.feedback.try_iter().last()
    }

    pub fn finish(self, revision: u64, editor: &Editor) -> io::Result<()> {
        if (revision, editor.cursor()) != self.queued {
            let note = CachedNote {
                text: editor.text().to_owned(),
                cursor: editor.cursor(),
            };
            let _ = self.queue.send((revision, note));
        }
        drop(self.queue);
        self.worker
            .join()
            .map_err(|_| io::Error::other("Note cache worker stopped"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_autosave_retries_the_note_and_caret_while_idle() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("cache");
        let path = directory.join("note.json");
        fs::write(&directory, "").unwrap();

        let mut cache = NoteCache::start(path.clone(), 0).unwrap();
        let editor = Editor::from_note("latest".into(), 2);
        cache.update(1, &editor);
        let timeout = Duration::from_secs(5);
        assert!(!cache.feedback.recv_timeout(timeout).unwrap());

        fs::remove_file(&directory).unwrap();
        assert!(cache.feedback.recv_timeout(timeout).unwrap());
        let restored = CachedNote::load(&path).unwrap().into_editor();
        assert_eq!(restored.text(), "latest");
        assert_eq!(restored.cursor(), 2);
        cache.finish(1, &editor).unwrap();
    }

    #[test]
    fn closing_flushes_the_latest_note_and_caret_and_can_clear_the_cache() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("cache/note.json");
        let mut editor = CachedNote::load(&path).unwrap().into_editor();
        assert!(!path.exists());
        let mut cache = NoteCache::start(path.clone(), editor.cursor()).unwrap();
        editor.insert("one");
        cache.update(1, &editor);
        editor.insert("\né界");
        editor.move_caret(crate::editor::Movement::Left, false, 80);
        cache.finish(2, &editor).unwrap();

        let mut restored = CachedNote::load(&path).unwrap().into_editor();
        assert_eq!(restored.text(), "one\né界");
        assert_eq!(restored.cursor(), "one\né".len());
        assert!(!restored.undo());
        let cache = NoteCache::start(path.clone(), restored.cursor()).unwrap();
        assert!(restored.clear());
        cache.finish(1, &restored).unwrap();
        assert!(
            CachedNote::load(&path)
                .unwrap()
                .into_editor()
                .text()
                .is_empty()
        );
    }

    #[test]
    fn caret_changes_cannot_replace_a_note_edited_in_another_window() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("cache/note.json");
        let mut editor = Editor::default();
        let cache = NoteCache::start(path.clone(), editor.cursor()).unwrap();
        editor.insert("original");
        cache.finish(1, &editor).unwrap();

        let stale = NoteCache::start(path.clone(), editor.cursor()).unwrap();
        let current = NoteCache::start(path.clone(), editor.cursor()).unwrap();
        current
            .finish(1, &Editor::from_note("updated".into(), 7))
            .unwrap();
        editor.move_caret(crate::editor::Movement::Left, false, 80);
        stale.finish(0, &editor).unwrap();
        assert_eq!(
            CachedNote::load(&path).unwrap().into_editor().text(),
            "updated"
        );
    }
}
