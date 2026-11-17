use super::*;
type Request = (Uuid, PathBuf, UiWork);

#[derive(Clone)]
pub struct Loader(std::sync::mpsc::Sender<Vec<Request>>);
impl Loader {
    pub fn new(events: UiSender<Event>) -> Self {
        let (sender, receiver) = std::sync::mpsc::channel::<Vec<Request>>();
        std::thread::spawn(move || {
            let mut queue = VecDeque::new();
            loop {
                if queue.is_empty() {
                    match receiver.recv() {
                        Ok(next) => queue = next.into(),
                        Err(_) => return,
                    }
                }
                // A new page replaces queued work before decoding the next image.
                while let Ok(next) = receiver.try_recv() {
                    queue = next.into();
                }
                if let Some((id, path, _work)) = queue.pop_front() {
                    let result = image::open(path)
                        .map(|image| image.thumbnail(360, 220).to_rgba8())
                        .map_err(Into::into);
                    events.send(Event::Thumbnail(id, result));
                }
            }
        });
        Self(sender)
    }
    pub fn replace(&self, requests: Vec<Request>) {
        let _ = self.0.send(requests);
    }
}
