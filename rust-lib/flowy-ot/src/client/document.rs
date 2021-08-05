use crate::{
    client::{History, RevId, Revision, UndoResult},
    core::{
        Attribute,
        Attributes,
        AttributesDataRule,
        AttrsBuilder,
        Delta,
        Interval,
        OpBuilder,
        Operation,
    },
    errors::{ErrorBuilder, OTError, OTErrorCode::*},
};

pub struct Document {
    data: Delta,
    history: History,
    rev_id_counter: u64,
}

impl Document {
    pub fn new() -> Self {
        Document {
            data: Delta::new(),
            history: History::new(),
            rev_id_counter: 1,
        }
    }

    pub fn edit(&mut self, index: usize, text: &str) {
        if self.data.target_len < index {
            log::error!(
                "{} out of bounds. should 0..{}",
                index,
                self.data.target_len
            );
        }
        let probe = Interval::new(index, index + 1);
        let mut attributes = self.data.get_attributes(probe);
        if attributes == Attributes::Empty {
            attributes = Attributes::Follow;
        }
        let insert = OpBuilder::insert(text).attributes(attributes).build();
        let interval = Interval::new(index, index);

        self.update_with_op(insert, interval);
    }

    pub fn format(&mut self, interval: Interval, attribute: Attribute, enable: bool) {
        let attributes = match enable {
            true => AttrsBuilder::new().add_attribute(attribute).build(),
            false => AttrsBuilder::new().remove_attribute(attribute).build(),
        };

        self.update_with_attribute(attributes, interval);
    }

    pub fn can_undo(&self) -> bool { self.history.can_undo() }

    pub fn can_redo(&self) -> bool { self.history.can_redo() }

    pub fn undo(&mut self) -> Result<UndoResult, OTError> {
        match self.history.undo() {
            None => Err(ErrorBuilder::new(UndoFail).build()),
            Some(undo_delta) => {
                let new_delta = self.data.compose(&undo_delta)?;
                let result = UndoResult::success(new_delta.target_len as u64);
                let redo_delta = undo_delta.invert_delta(&self.data);
                self.data = new_delta;
                self.history.add_redo(redo_delta);

                Ok(result)
            },
        }
    }

    pub fn redo(&mut self) -> Result<UndoResult, OTError> {
        match self.history.redo() {
            None => Err(ErrorBuilder::new(RedoFail).build()),
            Some(redo_delta) => {
                let new_delta = self.data.compose(&redo_delta)?;
                let result = UndoResult::success(new_delta.target_len as u64);
                let redo_delta = redo_delta.invert_delta(&self.data);
                self.data = new_delta;
                self.history.add_undo(redo_delta);
                Ok(result)
            },
        }
    }

    pub fn delete(&mut self, interval: Interval) {
        let delete = OpBuilder::delete(interval.size() as u64).build();
        self.update_with_op(delete, interval);
    }

    pub fn to_json(&self) -> String { self.data.to_json() }

    pub fn data(&self) -> &Delta { &self.data }

    pub fn set_data(&mut self, data: Delta) { self.data = data; }

    fn update_with_op(&mut self, op: Operation, interval: Interval) {
        let mut new_delta = Delta::default();
        let (prefix, interval, suffix) = split_length_with_interval(self.data.target_len, interval);

        // prefix
        if prefix.is_empty() == false && prefix != interval {
            let intervals = split_interval_with_delta(&self.data, &prefix);
            intervals.into_iter().for_each(|i| {
                let attributes = self.data.get_attributes(i);
                log::debug!("prefix attribute: {:?}, interval: {:?}", attributes, i);
                new_delta.retain(i.size() as u64, attributes);
            });
        }

        log::debug!("add new op: {:?}", op);
        new_delta.add(op);

        // suffix
        if suffix.is_empty() == false {
            let intervals = split_interval_with_delta(&self.data, &suffix);
            intervals.into_iter().for_each(|i| {
                let attributes = self.data.get_attributes(i);
                log::debug!("suffix attribute: {:?}, interval: {:?}", attributes, i);
                new_delta.retain(i.size() as u64, attributes);
            });
        }

        let new_data = self.data.compose(&new_delta).unwrap();
        let undo_delta = new_data.invert_delta(&self.data);
        self.rev_id_counter += 1;

        if !undo_delta.is_empty() {
            self.history.add_undo(undo_delta);
        }

        self.data = new_data;
    }

    pub fn update_with_attribute(&mut self, mut attributes: Attributes, interval: Interval) {
        let old_attributes = self.data.get_attributes(interval);
        log::debug!(
            "merge attributes: {:?}, with old: {:?}",
            attributes,
            old_attributes
        );
        let new_attributes = match &mut attributes {
            Attributes::Follow => old_attributes,
            Attributes::Custom(attr_data) => {
                attr_data.merge(old_attributes.data());
                attr_data.clone().into_attributes()
            },
            Attributes::Empty => Attributes::Empty,
        };

        log::debug!("new attributes: {:?}", new_attributes);
        let retain = OpBuilder::retain(interval.size() as u64)
            .attributes(new_attributes)
            .build();

        log::debug!(
            "Update delta with new attributes: {:?} at: {:?}",
            retain,
            interval
        );

        self.update_with_op(retain, interval);
    }

    fn next_rev_id(&self) -> RevId { RevId(self.rev_id_counter) }
}

pub fn transform(left: &mut Document, right: &mut Document) {
    let (a_prime, b_prime) = left.data.transform(&right.data).unwrap();
    log::trace!("a:{:?},b:{:?}", a_prime, b_prime);

    let data_left = left.data.compose(&b_prime).unwrap();
    let data_right = right.data.compose(&a_prime).unwrap();

    left.set_data(data_left);
    right.set_data(data_right);
}

fn split_length_with_interval(length: usize, interval: Interval) -> (Interval, Interval, Interval) {
    let original_interval = Interval::new(0, length);
    let prefix = original_interval.prefix(interval);
    let suffix = original_interval.suffix(interval);
    (prefix, interval, suffix)
}

fn split_interval_with_delta(delta: &Delta, interval: &Interval) -> Vec<Interval> {
    let mut start = 0;
    let mut new_intervals = vec![];
    delta.ops.iter().for_each(|op| match op {
        Operation::Delete(_) => {},
        Operation::Retain(_) => {},
        Operation::Insert(insert) => {
            let len = insert.num_chars() as usize;
            let end = start + len;
            let insert_interval = Interval::new(start, end);
            let new_interval = interval.intersect(insert_interval);

            if !new_interval.is_empty() {
                new_intervals.push(new_interval)
            }
            start += len;
        },
    });
    new_intervals
}