//! Phase 19 headless `egui::Context` UI smoke tests covering empty-state (`0 messages loaded`),
//! zero-match filtered state (`0 filtered results`), Filter bar multi-rule chips, Find & Replace,
//! Reports/Sort/Batch-Edit dialogs, Network MLLP docked tabs, and keyboard navigation shortcuts.

use pipe_wrench::ui::net_panels::TcpIpDockTab;
use pipe_wrench::{FilterCombineMode, PipeWrenchApp};

fn make_raw_input(events: Vec<egui::Event>, modifiers: egui::Modifiers) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1280.0, 800.0),
        )),
        modifiers,
        events,
        ..Default::default()
    }
}

fn key_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

fn run_frames(app: &mut PipeWrenchApp, ctx: &egui::Context, count: usize) {
    let raw = make_raw_input(Vec::new(), egui::Modifiers::NONE);
    for _ in 0..count {
        let _ = ctx.run(raw.clone(), |ctx| {
            app.render_frame(ctx);
        });
    }
}

#[test]
fn headless_empty_state_all_panels_dialogs_and_shortcuts_render_cleanly() {
    let mut app = PipeWrenchApp::new();
    assert!(app.state.doc.is_empty());
    let ctx = egui::Context::default();

    // 1. Baseline empty-state frames
    run_frames(&mut app, &ctx, 2);

    // 2. Filter bar: open, type query, add chip, toggle AND/OR, open ? help popover, evaluate 0 matches
    app.state.filter_bar_open = true;
    app.state.filter_help_open = true;
    app.state.filter_query = "PID-5.1=TESTPATIENT".to_string();
    app.state.focus_filter_input = true;
    run_frames(&mut app, &ctx, 2);
    assert!(!app.state.focus_filter_input);

    app.state.add_filter_from_input(&app.settings);
    app.state.filter_query = "MSH-9.2=A01".to_string();
    app.state.add_filter_from_input(&app.settings);
    assert_eq!(app.state.active_filters.len(), 2);
    assert_eq!(app.state.filtered_record_indices.as_deref(), Some(&[][..]));
    run_frames(&mut app, &ctx, 2);

    app.state
        .set_filter_combine_mode(FilterCombineMode::Or, &app.settings);
    run_frames(&mut app, &ctx, 2);

    // Keep Only Filtered on empty doc is a clean no-op
    assert!(!app.state.request_keep_only_filtered(&app.settings).unwrap());
    app.state.clear_filter(&app.settings);

    // 3. Find & Replace bar (Find tab, Replace tab, ? help popover, Count/Find Next on empty doc)
    app.state.open_find_panel();
    app.state.find_help_open = true;
    app.state.search_query = "PID".to_string();
    let _ = app.state.jump_search(true, &app.settings);
    run_frames(&mut app, &ctx, 2);
    app.state.open_replace_panel();
    run_frames(&mut app, &ctx, 2);
    app.state.close_find_replace_panel();

    // 4. Reports, Sort, Batch Edit (Assign Field / Change Delimiters), Preferences, Shortcuts, Scan
    app.state.report_window.open = true;
    app.state.open_sort_records_dialog();
    app.state.open_assign_field_dialog();
    app.state.open_change_delimiters_dialog();
    app.state.preferences_open = true;
    app.state.shortcuts_help_open = true;
    app.state.scan_window.open = true;
    run_frames(&mut app, &ctx, 2);

    // Close modals before testing Network tabs
    app.state.report_window.open = false;
    app.state.sort_records_dialog = None;
    app.state.assign_field_dialog = None;
    app.state.change_delimiters_dialog = None;
    app.state.preferences_open = false;
    app.state.shortcuts_help_open = false;
    app.state.scan_window.open = false;

    // 5. All 4 Network MLLP docked tabs on empty document
    for tab in [
        TcpIpDockTab::ClientSender,
        TcpIpDockTab::ServerListener,
        TcpIpDockTab::Connections,
        TcpIpDockTab::AckLog,
    ] {
        app.state.network.open_dock_tab(tab);
        run_frames(&mut app, &ctx, 2);
    }
    app.state.network.docked_open = false;

    // 6. Global shortcuts & navigation keys on empty document
    let ctrl = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    let ctrl_shift = egui::Modifiers {
        ctrl: true,
        command: true,
        shift: true,
        ..Default::default()
    };
    let shortcut_frames = [
        (egui::Key::F, ctrl),
        (egui::Key::H, ctrl),
        (egui::Key::F, ctrl_shift),
        (egui::Key::S, ctrl),
        (egui::Key::Z, ctrl),
        (egui::Key::Y, ctrl),
        (egui::Key::F2, egui::Modifiers::NONE),
        (egui::Key::ArrowDown, egui::Modifiers::NONE),
        (egui::Key::ArrowUp, egui::Modifiers::NONE),
        (egui::Key::PageDown, egui::Modifiers::NONE),
        (egui::Key::PageUp, egui::Modifiers::NONE),
        (egui::Key::Home, egui::Modifiers::NONE),
        (egui::Key::End, egui::Modifiers::NONE),
        (egui::Key::Escape, egui::Modifiers::NONE),
    ];
    for (key, mods) in shortcut_frames {
        let raw = make_raw_input(vec![key_event(key, mods)], mods);
        let _ = ctx.run(raw, |ctx| {
            app.render_frame(ctx);
        });
    }
}

#[test]
fn headless_zero_filtered_matches_state_renders_and_navigates_cleanly() {
    let mut app = PipeWrenchApp::new();
    let raw_hl7 = concat!(
        "MSH|^~\\&|APP|FAC|DST|DFAC|20260417||ADT^A01|MSG1|P|2.5.1\r",
        "PID|1||SYNTH0001^^^HOSP^MR||DOE^JANE||19800101|F\r",
        "MSH|^~\\&|APP|FAC|DST|DFAC|20260417||ORU^R01|MSG2|P|2.5.1\r",
        "PID|1||SYNTH0002^^^HOSP^MR||SMITH^JOHN||19750505|M\r",
    );
    app.state.paste_from_text(raw_hl7, false, &app.settings);
    assert_eq!(app.state.doc.len(), 2);

    let ctx = egui::Context::default();

    // Apply a filter that matches 0 records
    app.state
        .open_filter_for_path("PID-5.1", "NONEXISTENT_PATIENT", &app.settings);
    app.state
        .add_filter_for_path("MSH-9.2", "Z99", &app.settings);
    assert_eq!(app.state.filtered_record_indices.as_deref(), Some(&[][..]));
    assert_eq!(app.state.active_filters.len(), 2);

    // Render Filter bar + Find bar + Record list + Detail pane with 0 filtered matches
    app.state.open_find_panel();
    run_frames(&mut app, &ctx, 3);

    // Toggle OR (still 0 matches) and navigate keys on empty filtered list
    app.state
        .set_filter_combine_mode(FilterCombineMode::Or, &app.settings);
    for key in [
        egui::Key::ArrowDown,
        egui::Key::ArrowUp,
        egui::Key::PageDown,
        egui::Key::PageUp,
        egui::Key::Home,
        egui::Key::End,
    ] {
        let raw = make_raw_input(
            vec![key_event(key, egui::Modifiers::NONE)],
            egui::Modifiers::NONE,
        );
        let _ = ctx.run(raw, |ctx| {
            app.render_frame(ctx);
        });
    }

    // Clear filter restores all 2 messages
    app.state.clear_filter(&app.settings);
    assert!(app.state.filtered_record_indices.is_none());
    run_frames(&mut app, &ctx, 2);
}

#[test]
fn deleting_last_message_closes_overlays_disables_has_messages_and_undo_restores() {
    let mut app = PipeWrenchApp::new();
    app.settings.prompt_before_deleting = false;
    let raw_hl7 = concat!(
        "MSH|^~\\&|APP|FAC|DST|DFAC|20260417||ADT^A01|MSG1|P|2.5.1\r",
        "PID|1||SYNTH0001^^^HOSP^MR||DOE^JANE||19800101|F\r",
    );
    app.state.paste_from_text(raw_hl7, false, &app.settings);
    assert!(app.state.has_messages());

    // Open both Filter and Find bars while message is loaded
    app.state
        .open_filter_for_path("PID-5.1", "DOE", &app.settings);
    app.state.open_find_panel();
    assert!(app.state.filter_bar_open);
    assert!(app.state.search_bar_open);

    // Delete the only loaded message
    app.state
        .request_delete_records(vec![0], &app.settings)
        .unwrap();

    // Verify empty-state transition: overlays closed, filter cleared, has_messages == false, Undo preserved
    assert!(!app.state.has_messages());
    assert!(!app.state.filter_bar_open);
    assert!(!app.state.search_bar_open);
    assert!(app.state.filtered_record_indices.is_none());
    assert!(app.state.active_filters.is_empty());
    assert!(app.state.undo_stack.can_undo());

    // Render empty UI frames after deleting last message
    let ctx = egui::Context::default();
    run_frames(&mut app, &ctx, 2);

    // Undo restores the deleted message and re-enables message-dependent controls
    app.state.undo(&app.settings).unwrap();
    assert!(app.state.has_messages());
    assert_eq!(app.state.doc.len(), 1);
    assert_eq!(app.state.selected_record, Some(0));
    run_frames(&mut app, &ctx, 2);
}

#[test]
fn structured_filter_builder_wildcard_contains_and_exists_work_and_render() {
    use pipe_wrench::state::FilterBuilderOp;

    let mut app = PipeWrenchApp::new();
    let raw_hl7 = concat!(
        "MSH|^~\\&|APP|FAC|DST|DFAC|20260115120000||ADT^A01|MSG1|P|2.5.1\r",
        "PID|1||AAA^^^HOSP^MR~123^^^CLINIC^PI||DOE^JANE||19800101|F\r",
        "ROL|1|UP|AT|9999^HOUSE^GREGORY\r",
        "OBX|1|NM|GLU||95|mg/dL\r",
        "MSH|^~\\&|APP|FAC|DST|DFAC|20260417120000||ORU^R01|MSG2|P|2.5.1\r",
        "PID|1||999^^^HOSP^MR||SMITH^JOHN||19750505|M\r",
        "OBX|1|NM|GLU||240|mg/dL\r",
    );
    app.state.paste_from_text(raw_hl7, false, &app.settings);
    assert_eq!(app.state.doc.len(), 2);
    app.state.filter_bar_open = true;

    // 1. All 9 unified dropdown operators
    let cases: [(&str, FilterBuilderOp, &str, &[usize], &str); 9] = [
        (
            "PID-3(*).1",
            FilterBuilderOp::Equals,
            "123",
            &[0],
            "PID-3(*).1 = 123",
        ),
        (
            "PID-5.1",
            FilterBuilderOp::NotEquals,
            "SMITH",
            &[0],
            "PID-5.1 != SMITH",
        ),
        (
            "PID-3(*).1",
            FilterBuilderOp::Contains,
            "23",
            &[0],
            "PID-3(*).1 ~= 23",
        ),
        (
            "PID-5.1",
            FilterBuilderOp::Regex,
            "^SM.*",
            &[1],
            "PID-5.1 =~ ^SM.*",
        ),
        (
            "OBX-5",
            FilterBuilderOp::GreaterThan,
            "100",
            &[1],
            "OBX-5 > 100",
        ),
        (
            "MSH-7",
            FilterBuilderOp::GreaterOrEqual,
            "20260401000000",
            &[1],
            "MSH-7 >= 20260401000000",
        ),
        (
            "OBX-5",
            FilterBuilderOp::LessThan,
            "100",
            &[0],
            "OBX-5 < 100",
        ),
        (
            "MSH-7",
            FilterBuilderOp::LessOrEqual,
            "20260115120000",
            &[0],
            "MSH-7 <= 20260115120000",
        ),
        ("ROL-4", FilterBuilderOp::Exists, "", &[0], "ROL-4 exists"),
    ];
    for (path, op, val, expected_indices, expected_chip) in cases {
        app.state.clear_filter(&app.settings);
        app.state.filter_query = path.to_string();
        app.state.filter_builder_op = op;
        app.state.filter_builder_value = val.to_string();
        app.state.add_filter_from_input(&app.settings);
        assert_eq!(
            app.state.filtered_record_indices.as_deref(),
            Some(expected_indices),
            "op={:?} path={path} val={val}",
            op
        );
        assert_eq!(app.state.active_filters.len(), 1);
        assert_eq!(app.state.active_filters[0].query, expected_chip);
    }

    // 2. Exists hides value box (`requires_value() == false`)
    assert!(!FilterBuilderOp::Exists.requires_value());
    assert!(FilterBuilderOp::Regex.requires_value());

    // 3. Smart inline expression fallback in the single unified row (empty Value box)
    for (inline_expr, expected_indices) in [
        ("PID-3(*).1=123", &[0][..]),
        ("PID-5.1!=SMITH", &[0][..]),
        ("PID-5.1~=SMI", &[1][..]),
        ("PID-5.1=~^SM.*", &[1][..]),
        ("MSH-7>=20260401000000", &[1][..]),
        ("OBX-5<100", &[0][..]),
        ("ROL-4 exists", &[0][..]),
    ] {
        app.state.clear_filter(&app.settings);
        app.state.filter_builder_op = FilterBuilderOp::Equals;
        app.state.filter_query = inline_expr.to_string();
        app.state.filter_builder_value.clear();
        app.state.apply_filter(&app.settings);
        assert_eq!(
            app.state.filtered_record_indices.as_deref(),
            Some(expected_indices),
            "inline expr: {inline_expr}"
        );
    }

    // Render unified Filter bar in headless egui with Exists selected and active chip visible
    app.state.filter_builder_op = FilterBuilderOp::Exists;
    app.state.filter_query = "ROL-4".to_string();
    app.state.add_filter_from_input(&app.settings);
    let ctx = egui::Context::default();
    run_frames(&mut app, &ctx, 2);
}

#[test]
fn messages_footer_paste_button_renders_and_ingests_clipboard_hl7_with_undo() {
    let mut app = PipeWrenchApp::new();
    let ctx = egui::Context::default();

    // First frame to lay out the Messages footer toolbar (with + / Copy / Paste / Up / Down / Delete)
    run_frames(&mut app, &ctx, 2);
    assert_eq!(app.state.doc.len(), 0);

    // Locate the Paste button rect from its geometry in the docked bottom toolbar:
    // Left side panel has margin 8px, Plus (28px) + 6px + Copy (28px) + 6px => Paste center at x ~= 8 + 28 + 6 + 28 + 6 + 14 = 90.0
    // Bottom toolbar height is 24 + 12 = 36px at bottom of 800px screen (y ~= 782px, above status bar ~26px => y ~= 756px).
    // Even more deterministically: simulate clicking the Paste button via `egui::Event::Paste` + pointer click on its exact position,
    // AND verify `show_record_list` directly returns `RecordListAction::PasteFromClipboard` when clicked.
    let mut paste_btn_center = egui::pos2(90.0, 756.0);
    let _ = ctx.run(make_raw_input(Vec::new(), egui::Modifiers::NONE), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            let _ =
                pipe_wrench::ui::record_list::show_record_list(ui, &mut app.state, &app.settings);
            // Bottom toolbar is at the bottom of the panel (800px height):
            // inner_margin.left = 8, Plus = 28, spacing = 6, Copy = 28, spacing = 6, Paste = 28
            let r = ui.max_rect();
            paste_btn_center = egui::pos2(
                r.left() + 8.0 + 28.0 + 6.0 + 28.0 + 6.0 + 14.0,
                r.bottom() - 18.0,
            );
        });
    });

    // Click the Paste button with HL7 payload supplied via Event::Paste
    let pasted_hl7 = concat!(
        "MSH|^~\\&|CLIP|FAC|RCV|FAC|20261123120000||ADT^A01|CLIP1|P|2.5.1\r",
        "PID|1||98765^^^HOSP^MR||CLIPBOARD^PATIENT\r",
    );
    let mut clicked_action = pipe_wrench::ui::record_list::RecordListAction::None;
    for pressed in [true, false] {
        let events = vec![
            egui::Event::PointerMoved(paste_btn_center),
            egui::Event::PointerButton {
                pos: paste_btn_center,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        let _ = ctx.run(make_raw_input(events, egui::Modifiers::NONE), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let act = pipe_wrench::ui::record_list::show_record_list(
                    ui,
                    &mut app.state,
                    &app.settings,
                );
                if act != pipe_wrench::ui::record_list::RecordListAction::None {
                    clicked_action = act;
                }
            });
        });
    }
    assert_eq!(
        clicked_action,
        pipe_wrench::ui::record_list::RecordListAction::PasteFromClipboard
    );

    // Execute the exact same paste handler wired in PipeWrenchApp for RecordListAction::PasteFromClipboard
    app.state
        .paste_records_undoable(pasted_hl7, &app.settings)
        .unwrap();
    assert_eq!(app.state.doc.len(), 1);
    assert_eq!(app.state.selected_record, Some(0));
    assert!(app.state.undo_stack.can_undo());

    // Render full app frames (including full-width Segment Accordion headers)
    run_frames(&mut app, &ctx, 2);

    // Undo cleanly reverts the pasted message
    app.state.undo(&app.settings).unwrap();
    assert_eq!(app.state.doc.len(), 0);
}
