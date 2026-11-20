use std::{
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
    time::Duration,
};
use tokio::sync::oneshot;
use unicode_segmentation::UnicodeSegmentation;

pub struct Clipboard {
    write: Option<SyncSender<ClipboardWrite>>,
    pub feedback: Receiver<ClipboardFeedback>,
    worker: Option<thread::JoinHandle<()>>,
}

pub struct ClipboardWrite {
    pub text: String,
    pub reply: Option<oneshot::Sender<ClipboardFeedback>>,
}

#[derive(Clone, Copy)]
pub enum ClipboardFeedback {
    Copied(usize),
    Failed,
}

impl Clipboard {
    pub fn new() -> Self {
        let (write, notes) = mpsc::sync_channel(4);
        let (report, feedback) = mpsc::channel();
        let worker = thread::spawn(move || publish(notes, report));
        Self {
            write: Some(write),
            feedback,
            worker: Some(worker),
        }
    }

    pub fn copy(&self, text: &str, reply: Option<oneshot::Sender<ClipboardFeedback>>) -> bool {
        self.write.as_ref().is_some_and(|write| {
            write
                .try_send(ClipboardWrite {
                    text: text.to_owned(),
                    reply,
                })
                .is_ok()
        })
    }

    #[cfg(test)]
    pub fn test_sink() -> (
        Self,
        Receiver<ClipboardWrite>,
        mpsc::Sender<ClipboardFeedback>,
    ) {
        let (write, notes) = mpsc::sync_channel(4);
        let (report, feedback) = mpsc::channel();
        (
            Self {
                write: Some(write),
                feedback,
                worker: None,
            },
            notes,
            report,
        )
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        drop(self.write.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn publish(notes: Receiver<ClipboardWrite>, report: mpsc::Sender<ClipboardFeedback>) {
    let mut clipboard = None;
    while let Ok(request) = notes.recv() {
        if request
            .reply
            .as_ref()
            .is_some_and(|reply| reply.is_closed())
        {
            continue;
        }
        let mut result = ClipboardFeedback::Failed;
        for attempt in 0..5 {
            if attempt != 0 {
                thread::sleep(Duration::from_millis(20));
            }
            if clipboard.is_none() {
                clipboard = arboard::Clipboard::new().ok();
            }
            if clipboard
                .as_mut()
                .is_some_and(|clipboard| clipboard.set_text(request.text.as_str()).is_ok())
            {
                result = ClipboardFeedback::Copied(request.text.graphemes(true).count());
                break;
            }
        }
        if let Some(reply) = request.reply {
            let _ = reply.send(result);
        }
        let _ = report.send(result);
    }
}
