use super::*;
use anti_webui::core::{
    Ui,
    event::{
        ImeUpdate, KeyEvent, Modifiers, PointerButton, ScrollPhase, ScrollSource, WindowEvent,
    },
    geometry::{PointF, Scale, SizeI},
};

fn setup() -> (Ui, Controller, std::path::PathBuf) {
    let base = std::env::temp_dir().join(format!("ylp2-input-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&base).unwrap();
    // Saving intentionally fails fast; input tests never write an actual dataset.
    let blocked = base.join("blocked");
    std::fs::write(&blocked, "file").unwrap();
    let mut p = Project::new(blocked.join("input.ylp2"), vec!["H".into()]).unwrap();
    for index in 0..40 {
        p.samples.push(serde_json::from_value(serde_json::json!({
            "id":Uuid::new_v4(),"batch":Uuid::nil(),"path":format!("frame{index}.png"),"origin_root":".","origin_relative":format!("frame{index}.png"),
            "source_hash":"test","width":160,"height":100,"boxes":[{"class_id":0,"cx":0.5,"cy":0.5,"width":0.2,"height":0.2}],
            "has_label":true,"label_errors":[],"image_error":null,"decision":"keep","reviewed":false,"reason":"","proposals":[],"proposal_pending":false,
            "proposal_info":null,"quality":null,"duplicate_of":null,"duplicate_conflict":false
        })).unwrap());
    }
    let mut ui = Ui::new(move || app_with(Some(p), Some(false), false));
    ui.handle_event(WindowEvent::Resized {
        size: SizeI::new(1480, 940),
        scale: Scale::from_f64(1.),
    });
    ui.handle_event(WindowEvent::Focused(true));
    ui.update();
    ui.update();
    let c = QA_CONTROLLER.with(|v| v.borrow().as_ref().unwrap().clone());
    (ui, c, base)
}
fn key(ui: &mut Ui, key: Key, ctrl: bool) {
    for pressed in [true, false] {
        ui.handle_event(WindowEvent::Key(KeyEvent {
            key: key.clone(),
            text: if ctrl {
                None
            } else {
                match &key {
                    Key::Character(text) => Some(text.clone()),
                    _ => None,
                }
            },
            modifiers: Modifiers {
                ctrl,
                ..Default::default()
            },
            pressed,
            repeat: false,
        }));
    }
    ui.update();
}
fn page(c: &Controller) -> usize {
    c.state.borrow().project.as_ref().unwrap().view.page
}
fn cleanup(ui: Ui, c: Controller, path: std::path::PathBuf) {
    c.saver.close();
    drop(ui);
    drop(c);
    let _ = std::fs::remove_dir_all(path);
}
#[test]
fn keyboard_navigation_immediate_toggle_and_undo() {
    let (mut ui, c, path) = setup();
    key(&mut ui, Key::Character("d".into()), false);
    assert_eq!(
        page(&c),
        1,
        "search={}, allowed={}",
        c.search.get_untracked(),
        c.allowed()
    );
    key(&mut ui, Key::Character("a".into()), false);
    assert_eq!(page(&c), 0);
    key(&mut ui, Key::Named(NamedKey::ArrowRight), false);
    assert_eq!(page(&c), 1);
    key(&mut ui, Key::Named(NamedKey::ArrowLeft), false);
    assert_eq!(page(&c), 0);
    let id = c.page_ids()[0];
    key(&mut ui, Key::Character("1".into()), false);
    assert_eq!(
        c.state
            .borrow()
            .project
            .as_ref()
            .unwrap()
            .samples
            .iter()
            .find(|s| s.id == id)
            .unwrap()
            .decision,
        Decision::Exclude
    );
    key(&mut ui, Key::Character("z".into()), true);
    assert_eq!(
        c.state
            .borrow()
            .project
            .as_ref()
            .unwrap()
            .samples
            .iter()
            .find(|s| s.id == id)
            .unwrap()
            .decision,
        Decision::Keep
    );
    for time in [100, 160] {
        for pressed in [true, false] {
            ui.handle_event(WindowEvent::PointerButton {
                position: PointF::new(100., 200.),
                button: PointerButton::Primary,
                pressed,
                modifiers: Modifiers::default(),
                time,
            });
        }
        ui.update();
    }
    assert_eq!(
        c.state
            .borrow()
            .project
            .as_ref()
            .unwrap()
            .samples
            .iter()
            .find(|s| s.id == id)
            .unwrap()
            .decision,
        Decision::Keep
    );
    key(&mut ui, Key::Named(NamedKey::Enter), false);
    assert_eq!(page(&c), 1);
    cleanup(ui, c, path);
}
#[test]
fn wheel_gesture_changes_one_page_and_respects_boundaries() {
    let (mut ui, c, path) = setup();
    let scroll = |delta| WindowEvent::Scroll {
        position: PointF::new(200., 200.),
        delta_x: 0.,
        delta_y: delta,
        modifiers: Modifiers::default(),
        source: ScrollSource::Wheel,
        phase: ScrollPhase::Single,
        time: 10,
    };
    ui.handle_event(scroll(1.));
    ui.update();
    assert_eq!(page(&c), 1);
    ui.handle_event(scroll(1.));
    ui.update();
    assert_eq!(page(&c), 1);
    c.state.borrow_mut().last_wheel = None;
    ui.handle_event(scroll(-1.));
    ui.update();
    assert_eq!(page(&c), 0);
    c.state.borrow_mut().last_wheel = None;
    ui.handle_event(scroll(-1.));
    ui.update();
    assert_eq!(page(&c), 0);
    cleanup(ui, c, path);
}
#[test]
fn text_input_and_ime_do_not_toggle_images_or_turn_pages() {
    let (mut ui, c, path) = setup();
    c.page(1);
    ui.update();
    for pressed in [true, false] {
        ui.handle_event(WindowEvent::PointerButton {
            position: PointF::new(100., 70.),
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::default(),
            time: 100,
        });
    }
    ui.update();
    key(&mut ui, Key::Named(NamedKey::ArrowRight), false);
    assert_eq!(page(&c), 1);
    key(&mut ui, Key::Character("a".into()), false);
    key(&mut ui, Key::Character("1".into()), false);
    assert_eq!(c.search.get_untracked(), "a1");
    ui.handle_event(WindowEvent::Ime(ImeUpdate {
        preedit: Some(("へんかん".into(), None)),
        ..Default::default()
    }));
    ui.update();
    ui.handle_event(WindowEvent::Ime(ImeUpdate {
        commit: Some("変換".into()),
        ..Default::default()
    }));
    ui.update();
    assert!(c.search.get_untracked().contains("変換"));
    assert!(
        c.state
            .borrow()
            .project
            .as_ref()
            .unwrap()
            .samples
            .iter()
            .all(|s| s.decision == Decision::Keep)
    );
    cleanup(ui, c, path);
}
#[test]
fn obsolete_save_completion_cannot_clear_newer_dirty_state() {
    let (mut ui, c, path) = setup();
    let id = c.state.borrow().project.as_ref().unwrap().id;
    {
        let mut state = c.state.borrow_mut();
        state.save_generation = 20;
        state.dirty = true;
    }
    c.events.send(Event::Saved(id, 19, Ok(())));
    ui.poll_images();
    ui.update();
    assert!(c.state.borrow().dirty);
    c.events.send(Event::Saved(id, 20, Err("disk full".into())));
    ui.poll_images();
    ui.update();
    assert!(c.state.borrow().dirty);
    assert!(c.status.get_untracked().contains("未保存"));
    c.events.send(Event::Saved(id, 20, Ok(())));
    ui.poll_images();
    ui.update();
    assert!(!c.state.borrow().dirty);
    cleanup(ui, c, path);
}

#[test]
fn intake_requested_from_a_closing_dialog_belongs_to_the_main_window() {
    let (ui, c, path) = setup();
    let parent = ui.window();
    let mut child = Ui::new(div);
    child.update();
    assert_ne!(child.window(), parent);
    c.events
        .send(Event::Intake(None, path.clone(), vec![], None));
    child.poll_images();
    let requests = anti_webui::core::window::take_opens();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].parent.as_ref().unwrap().id(), parent);
    drop(requests);
    drop(child);
    cleanup(ui, c, path);
}

#[test]
fn added_batch_appears_after_existing_images_in_default_order() {
    let (ui, c, path) = setup();
    let existing = c.visible();
    let batch = Uuid::new_v4();
    {
        let mut state = c.state.borrow_mut();
        let samples = &mut state.project.as_mut().unwrap().samples;
        for filename in ["frame10.png", "frame2.png", "0.png"] {
            let mut sample = samples[0].clone();
            sample.id = Uuid::new_v4();
            sample.batch = batch;
            sample.origin_relative = filename.into();
            samples.push(sample);
        }
    }
    let visible = c.visible();
    assert_eq!(&visible[..existing.len()], &existing);
    assert_eq!(&visible[existing.len()..], &[42, 41, 40]);
    c.sort.set(4);
    assert_eq!(c.visible()[0], 42);
    cleanup(ui, c, path);
}

#[test]
fn held_navigation_key_repeats_until_released() {
    use std::time::{Duration, Instant};
    let (mut ui, c, path) = setup();
    let arrow = |pressed, repeat| {
        WindowEvent::Key(KeyEvent {
            key: Key::Named(NamedKey::ArrowRight),
            text: None,
            modifiers: Modifiers::default(),
            pressed,
            repeat,
        })
    };
    ui.handle_event(arrow(true, false));
    assert_eq!(page(&c), 1);
    ui.handle_event(arrow(true, true));
    assert_eq!(page(&c), 1);
    let start = Instant::now();
    ui.animate(start + Duration::from_millis(500));
    assert_eq!(page(&c), 2);
    ui.handle_event(arrow(false, false));
    ui.animate(start + Duration::from_secs(3));
    assert_eq!(page(&c), 2);
    cleanup(ui, c, path);
}

#[test]
fn page_button_hold_stops_on_release_or_focus_loss() {
    use std::time::{Duration, Instant};
    let (mut ui, c, path) = setup();
    let button = |pressed| WindowEvent::PointerButton {
        position: PointF::new(230., 885.),
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::default(),
        time: 10,
    };
    ui.handle_event(button(true));
    assert_eq!(page(&c), 1);
    let start = Instant::now();
    ui.animate(start + Duration::from_millis(500));
    assert_eq!(page(&c), 2);
    ui.handle_event(button(false));
    ui.animate(start + Duration::from_secs(1));
    assert_eq!(page(&c), 2);
    ui.handle_event(button(true));
    assert_eq!(page(&c), 3);
    ui.handle_event(WindowEvent::Focused(false));
    assert!(c.page_hold.borrow().is_none());
    cleanup(ui, c, path);
}

#[test]
fn opening_project_shows_thumbnails_after_clearing_saved_search() {
    let dir = std::env::temp_dir().join(format!("ylp2-filter-{}", Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("saved.ylp2");
    let source = dir.join("source");
    std::fs::create_dir(&source).unwrap();
    let image = source.join("sample.png");
    image::RgbImage::from_pixel(10, 10, image::Rgb([20, 40, 60]))
        .save(&image)
        .unwrap();
    let mut p = Project::new(path.clone(), vec!["H".into()]).unwrap();
    p.import(
        &source,
        &[image],
        None,
        &AtomicBool::new(false),
        &mut |_, _| {},
    )
    .unwrap();
    let id = p.samples[0].id;
    p.view.search = "一致しない".into();
    p.save().unwrap();

    let mut ui = Ui::new(|| app_with(None, Some(false), false));
    ui.update();
    let c = QA_CONTROLLER.with(|v| v.borrow().as_ref().unwrap().clone());
    c.job(Job::Load(path.clone()));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while c.busy.get_untracked() && std::time::Instant::now() < deadline {
        ui.update();
        ui.wait_for_images(std::time::Duration::from_millis(20));
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!c.busy.get_untracked());
    assert!(c.job_error.get_untracked().is_none());
    assert_eq!(c.state.borrow().project.as_ref().unwrap().samples.len(), 1);
    assert!(c.visible().is_empty());
    c.clear_filters();
    ui.update();
    ui.wait_for_images(std::time::Duration::from_secs(3));
    assert_eq!(c.page_ids(), vec![id]);
    assert!(
        c.state
            .borrow()
            .thumbnail_slots
            .get(&id)
            .unwrap()
            .bitmap
            .get_untracked()
            .is_some()
    );
    assert!(c.flush());
    cleanup(ui, c, dir);
}

#[test]
fn page_selection_toggles_visible_page_and_keeps_other_pages_selected() {
    let (mut ui, c, path) = setup();
    let first = c.page_ids();
    c.toggle_selection(first[0]);
    c.toggle_page_selection();
    assert_eq!(c.state.borrow().selection, first.iter().copied().collect());
    c.page(1);
    let second = c.page_ids();
    c.toggle_page_selection();
    let expected = first.iter().chain(&second).copied().collect();
    assert_eq!(c.state.borrow().selection, expected);
    c.toggle_page_selection();
    assert_eq!(c.state.borrow().selection, first.iter().copied().collect());
    c.search.set("frame1".into());
    ui.update();
    assert!(c.state.borrow().selection.is_empty());
    let filtered = c.page_ids();
    c.toggle_page_selection();
    assert_eq!(
        c.state.borrow().selection,
        filtered.iter().copied().collect()
    );
    c.clear_selection();
    assert!(c.state.borrow().selection.is_empty());
    cleanup(ui, c, path);
}
