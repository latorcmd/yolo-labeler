use labeler_core::Project;
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, mpsc},
    time::Duration,
};
struct Request {
    project: Project,
    generation: u64,
    reply: Option<mpsc::Sender<Result<(), String>>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scratch() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("ylp2-writer-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    #[test]
    fn flush_writes_latest_coalesced_snapshot() {
        let path = scratch();
        let mut project = Project::new(path.join("project.ylp2"), vec!["H".into()]).unwrap();
        let saver = Saver::new(|_, _, _| {});
        for generation in 1..50 {
            project.view.page = generation;
            saver
                .submit(project.save_snapshot(), generation as u64)
                .unwrap();
        }
        project.view.page = 99;
        saver.flush(project.save_snapshot(), 99).unwrap();
        assert_eq!(Project::load(&project.file).unwrap().view.page, 99);
        saver.close();
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn failed_save_can_retry_without_losing_latest_state() {
        let path = scratch();
        let blocked = path.join("blocked");
        std::fs::write(&blocked, "file").unwrap();
        let mut project = Project::new(blocked.join("project.ylp2"), vec!["H".into()]).unwrap();
        let saver = Saver::new(|_, _, _| {});
        assert!(saver.flush(project.save_snapshot(), 1).is_err());
        std::fs::remove_file(&blocked).unwrap();
        project.view.search = "再保存".into();
        saver.flush(project.save_snapshot(), 2).unwrap();
        assert_eq!(Project::load(&project.file).unwrap().view.search, "再保存");
        saver.close();
        std::fs::remove_dir_all(path).unwrap();
    }
}
struct Shared {
    queue: Mutex<VecDeque<Request>>,
    ready: Condvar,
    closed: std::sync::atomic::AtomicBool,
}
/// One disk writer, with at most one queued snapshot per active project.
#[derive(Clone)]
pub struct Saver(Arc<Shared>);
impl Saver {
    pub fn new(done: impl Fn(uuid::Uuid, u64, Result<(), String>) + Send + 'static) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            ready: Condvar::new(),
            closed: std::sync::atomic::AtomicBool::new(false),
        });
        let worker = shared.clone();
        std::thread::spawn(move || {
            loop {
                let request = {
                    let mut queue = worker.queue.lock().unwrap();
                    while queue.is_empty()
                        && !worker.closed.load(std::sync::atomic::Ordering::Relaxed)
                    {
                        queue = worker.ready.wait(queue).unwrap();
                    }
                    if queue.is_empty() {
                        return;
                    }
                    while !worker.closed.load(std::sync::atomic::Ordering::Relaxed)
                        && !queue.iter().any(|request| request.reply.is_some())
                    {
                        let (next, timeout) = worker
                            .ready
                            .wait_timeout(queue, Duration::from_millis(250))
                            .unwrap();
                        queue = next;
                        if timeout.timed_out() {
                            break;
                        }
                    }
                    queue.pop_front().unwrap()
                };
                let result = request.project.save().map_err(|error| format!("{error:#}"));
                done(request.project.id, request.generation, result.clone());
                if let Some(reply) = request.reply {
                    let _ = reply.send(result);
                }
            }
        });
        Self(shared)
    }
    fn enqueue(&self, request: Request) {
        let mut queue = self.0.queue.lock().unwrap();
        if queue
            .back()
            .is_some_and(|last| last.project.id == request.project.id && last.reply.is_none())
        {
            queue.pop_back();
        }
        queue.push_back(request);
        drop(queue);
        self.0.ready.notify_one();
    }
    pub fn submit(&self, project: Project, generation: u64) -> Result<(), String> {
        self.enqueue(Request {
            project,
            generation,
            reply: None,
        });
        Ok(())
    }
    pub fn close(&self) {
        let _queue = self.0.queue.lock().unwrap();
        self.0
            .closed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.0.ready.notify_one();
    }
    pub fn flush(&self, project: Project, generation: u64) -> Result<(), String> {
        let (sender, receiver) = mpsc::channel();
        self.enqueue(Request {
            project,
            generation,
            reply: Some(sender),
        });
        receiver.recv().map_err(|error| error.to_string())?
    }
}
