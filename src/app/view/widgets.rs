//! Small widgets shared by the settings panels.

use std::ops::RangeInclusive;

use eframe::egui;

const STEPS: [(&str, i32); 4] = [("−10", -10), ("−1", -1), ("+1", 1), ("+10", 10)];

/// What the user asked for through a value widget this frame.
#[derive(Debug, Default)]
pub(super) struct Edit {
    /// A new value to apply immediately.
    pub value: Option<i32>,
    /// The edit is finished and should be saved.
    pub persist: bool,
}

/// A draggable integer field. `dirty` remembers an unsaved edit so the setting
/// is saved once the drag or text entry ends, not on every intermediate value.
pub(super) fn drag_value(
    ui: &mut egui::Ui,
    value: i32,
    range: RangeInclusive<i32>,
    dirty: &mut bool,
) -> Edit {
    let mut edited = value;
    let response = ui.add(egui::DragValue::new(&mut edited).speed(1.0).range(range));
    let mut edit = Edit::default();
    if response.changed() {
        *dirty = true;
        edit.value = Some(edited);
    }
    if *dirty && (response.drag_stopped() || response.lost_focus()) {
        *dirty = false;
        edit.persist = true;
    }
    edit
}

/// A labelled percentage field with −10/−1/+1/+10 buttons.
pub(super) fn percent_row(
    ui: &mut egui::Ui,
    label: &str,
    value: i32,
    range: RangeInclusive<i32>,
    dirty: &mut bool,
) -> Edit {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut edit = drag_value(ui, value, range.clone(), dirty);
        ui.label("%");
        for (text, delta) in STEPS {
            if ui.button(text).clicked() {
                let stepped = (value + delta).clamp(*range.start(), *range.end());
                edit = Edit {
                    value: Some(stepped),
                    persist: true,
                };
            }
        }
        edit
    })
    .inner
}

pub(super) fn error_label(ui: &mut egui::Ui, text: impl Into<egui::RichText>) {
    ui.colored_label(ui.visuals().error_fg_color, text.into());
}

pub(super) fn warn_label(ui: &mut egui::Ui, text: impl Into<egui::RichText>) {
    ui.colored_label(ui.visuals().warn_fg_color, text.into());
}
