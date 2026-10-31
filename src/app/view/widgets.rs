//! Small widgets shared by the settings panels.

use std::ops::RangeInclusive;

use eframe::egui;

const STEPS: [(&str, i32); 4] = [("−10", -10), ("−1", -1), ("+1", 1), ("+10", 10)];

/// What the user asked for through a value widget this frame.
#[derive(Debug)]
pub(super) struct Edit<T> {
    /// A new value to apply immediately.
    pub value: Option<T>,
    /// The edit is finished and should be saved.
    pub persist: bool,
    /// The value still has an edit that is not saved yet.
    pub dirty: bool,
}

/// A draggable integer field. `dirty` says whether an unsaved edit is pending, so the
/// setting is saved once the drag or text entry ends, not on every intermediate value.
pub(super) fn drag_value<T>(
    ui: &mut egui::Ui,
    value: T,
    range: RangeInclusive<T>,
    dirty: bool,
) -> Edit<T>
where
    T: Copy + Into<i32> + TryFrom<i32>,
{
    let low: i32 = (*range.start()).into();
    let high: i32 = (*range.end()).into();
    let mut edited: i32 = value.into();
    let response = ui.add(
        egui::DragValue::new(&mut edited)
            .speed(1.0)
            .range(low..=high),
    );
    let mut edit = Edit {
        value: None,
        persist: false,
        dirty,
    };
    if response.changed() {
        edit.dirty = true;
        // The drag value is clamped to `range`, so the conversion back succeeds.
        edit.value = T::try_from(edited).ok();
    }
    if edit.dirty && (response.drag_stopped() || response.lost_focus()) {
        edit.dirty = false;
        edit.persist = true;
    }
    edit
}

/// A labelled percentage field with −10/−1/+1/+10 buttons.
pub(super) fn percent_row<T>(
    ui: &mut egui::Ui,
    label: &str,
    value: T,
    range: RangeInclusive<T>,
    dirty: bool,
) -> Edit<T>
where
    T: Copy + Into<i32> + TryFrom<i32>,
{
    ui.horizontal(|ui| {
        ui.label(label);
        let mut edit = drag_value(ui, value, range.clone(), dirty);
        ui.label("%");
        let low: i32 = (*range.start()).into();
        let high: i32 = (*range.end()).into();
        let current: i32 = value.into();
        for (text, delta) in STEPS {
            if ui.button(text).clicked() {
                let stepped = (current + delta).clamp(low, high);
                edit = Edit {
                    value: T::try_from(stepped).ok(),
                    persist: true,
                    dirty: false,
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
