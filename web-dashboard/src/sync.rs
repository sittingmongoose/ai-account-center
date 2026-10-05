//! In-place model updates. The dashboard keeps one persistent `VecModel` per list and per nested list
//! (keyed by id), and updates rows with `set_row_data` only when they changed. Slint's repeaters then
//! keep their element instances, so a changed reading animates from its previous value instead of the
//! element being re-created at its final value.

use slint::{Model, ModelRc, SharedString, VecModel};
use std::{
    collections::{HashMap, VecDeque},
    rc::Rc,
};

/// Make `model` equal to `rows`, matching rows by `key`. Rows whose key is unchanged and whose data is
/// equal are left alone; changed rows are replaced in place; new rows are inserted at their position;
/// rows that disappeared are removed.
pub fn sync_rows<T: Clone + PartialEq + 'static>(
    model: &VecModel<T>,
    rows: Vec<T>,
    key: impl Fn(&T) -> SharedString,
) {
    // Where every key sat before the first edit. The candidates are always the rows after the last
    // match, in their original order, so this one pass replaces a scan per wanted row.
    let mut positions: HashMap<SharedString, VecDeque<usize>> = HashMap::new();
    for at in 0..model.row_count() {
        if let Some(current) = model.row_data(at) {
            positions.entry(key(&current)).or_default().push_back(at);
        }
    }
    let mut index = 0;
    let mut matched: isize = -1;
    for row in rows {
        let found = match positions.get_mut(&key(&row)) {
            Some(queue) => {
                // Anything at or before the last match is no longer in the model.
                while queue.front().is_some_and(|&at| at as isize <= matched) {
                    queue.pop_front();
                }
                queue.pop_front()
            }
            None => None,
        };
        match found {
            Some(at) => {
                // Rows between the cursor and the match were removed or moved later.
                for _ in 0..at as isize - matched - 1 {
                    model.remove(index);
                }
                if model.row_data(index).as_ref() != Some(&row) {
                    model.set_row_data(index, row);
                }
                matched = at as isize;
            }
            None => model.insert(index, row),
        }
        index += 1;
    }
    while model.row_count() > index {
        model.remove(model.row_count() - 1);
    }
}

/// A family of persistent nested models, one per owner id (for example the meter cells of each row).
pub struct Nested<T: 'static> {
    models: HashMap<String, Rc<VecModel<T>>>,
}

impl<T: Clone + PartialEq + 'static> Default for Nested<T> {
    fn default() -> Self {
        Self {
            models: HashMap::new(),
        }
    }
}

impl<T: Clone + PartialEq + 'static> Nested<T> {
    /// Sync the nested model of `owner` and return a handle to the same persistent model.
    pub fn sync(
        &mut self,
        owner: &str,
        rows: Vec<T>,
        key: impl Fn(&T) -> SharedString,
    ) -> ModelRc<T> {
        let model = self
            .models
            .entry(owner.to_string())
            .or_insert_with(|| Rc::new(VecModel::default()))
            .clone();
        sync_rows(&model, rows, key);
        model.into()
    }

    /// Drop the models of owners that are gone.
    pub fn retain(&mut self, live: &[String]) {
        self.models.retain(|owner, _| live.contains(owner));
    }
}
