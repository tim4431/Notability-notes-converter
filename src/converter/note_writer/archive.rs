//! Keyed archives: `$objects` graphs whose references are UIDs, as written by
//! Foundation's `NSKeyedArchiver` and Notability's `GLKeyedArchiver` subclass.
use crate::Result;
use plist::{Dictionary, Uid, Value};
use std::collections::HashMap;

pub(super) struct ArchiveBuilder {
    archiver: &'static str,
    top_key: &'static str,
    objects: Vec<Value>,
    classes: HashMap<&'static str, Value>,
    /// Equal strings and numbers share one object, as the archivers write them.
    interned: HashMap<Interned, Value>,
}

#[derive(Eq, Hash, PartialEq)]
enum Interned {
    String(String),
    Integer(i64),
    Real(u64),
    Boolean(bool),
}

impl ArchiveBuilder {
    pub(super) fn new(archiver: &'static str, top_key: &'static str) -> Self {
        Self {
            archiver,
            top_key,
            objects: vec![Value::String("$null".to_owned())],
            classes: HashMap::new(),
            interned: HashMap::new(),
        }
    }

    pub(super) fn add(&mut self, value: Value) -> Value {
        self.objects.push(value);
        Value::Uid(Uid::new(self.objects.len() as u64 - 1))
    }

    /// A slot filled later, so that a parent can precede its children.
    pub(super) fn reserve(&mut self) -> Value {
        self.add(Value::Boolean(false))
    }

    /// A scalar as a shared object, for containers whose entries must be references.
    pub(super) fn scalar(&mut self, value: Value) -> Value {
        let key = match &value {
            Value::String(text) => Interned::String(text.clone()),
            Value::Integer(number) => match number.as_signed() {
                Some(number) => Interned::Integer(number),
                None => return self.add(value),
            },
            Value::Real(number) => Interned::Real(number.to_bits()),
            Value::Boolean(flag) => Interned::Boolean(*flag),
            _ => return self.add(value),
        };
        if let Some(uid) = self.interned.get(&key) {
            return uid.clone();
        }
        let uid = self.add(value);
        self.interned.insert(key, uid.clone());
        uid
    }

    pub(super) fn string(&mut self, text: &str) -> Value {
        self.scalar(Value::String(text.to_owned()))
    }

    pub(super) fn mutable_string(&mut self, text: &str) -> Value {
        self.object(
            "NSMutableString",
            vec![("NS.bytes", Value::Data(text.as_bytes().to_vec()))],
        )
    }

    pub(super) fn object(&mut self, class: &'static str, fields: Vec<(&str, Value)>) -> Value {
        let slot = self.reserve();
        self.fill_object(&slot, class, fields);
        slot
    }

    pub(super) fn fill_object(
        &mut self,
        slot: &Value,
        class: &'static str,
        fields: Vec<(&str, Value)>,
    ) {
        let class = self.class(class);
        let mut dictionary = Dictionary::new();
        for (key, value) in fields {
            dictionary.insert(key.to_owned(), value);
        }
        dictionary.insert("$class".to_owned(), class);
        let index = slot.as_uid().expect("archive slots are UIDs").get() as usize;
        self.objects[index] = Value::Dictionary(dictionary);
    }

    pub(super) fn array(&mut self, items: Vec<Value>) -> Value {
        self.object("NSArray", vec![("NS.objects", Value::Array(items))])
    }

    pub(super) fn mutable_array(&mut self, items: Vec<Value>) -> Value {
        self.object("NSMutableArray", vec![("NS.objects", Value::Array(items))])
    }

    /// `NS.keys`/`NS.objects` dictionary; both keys and values are references.
    pub(super) fn dictionary(&mut self, mutable: bool, entries: Vec<(Value, Value)>) -> Value {
        let (keys, values): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
        self.object(
            if mutable {
                "NSMutableDictionary"
            } else {
                "NSDictionary"
            },
            vec![
                ("NS.keys", Value::Array(keys)),
                ("NS.objects", Value::Array(values)),
            ],
        )
    }

    /// A dictionary with string keys.
    pub(super) fn string_dictionary(
        &mut self,
        mutable: bool,
        entries: Vec<(&str, Value)>,
    ) -> Value {
        let entries = entries
            .into_iter()
            .map(|(key, value)| (self.string(key), value))
            .collect();
        self.dictionary(mutable, entries)
    }

    pub(super) fn date(&mut self, unix_ms: i64) -> Value {
        self.object(
            "NSDate",
            vec![("NS.time", Value::Real(super::apple_seconds(unix_ms)))],
        )
    }

    pub(super) fn uuid(&mut self, bytes: [u8; 16]) -> Value {
        self.object(
            "NSUUID",
            vec![("NS.uuidbytes", Value::Data(bytes.to_vec()))],
        )
    }

    /// `UIColor` in the RGB form that Notability writes.
    pub(super) fn color(&mut self, rgba: [u8; 4]) -> Value {
        let components = rgba.map(|value| f64::from(value) / 255.0);
        let mut fields = vec![
            ("UIColorComponentCount", integer(4)),
            ("NSColorSpace", integer(2)),
        ];
        let mut doubles = Vec::new();
        for (key, double_key, value) in [
            ("UIRed", "UIRed-Double", components[0]),
            ("UIGreen", "UIGreen-Double", components[1]),
            ("UIBlue", "UIBlue-Double", components[2]),
            ("UIAlpha", "UIAlpha-Double", components[3]),
        ] {
            let single = f64::from(value as f32);
            fields.push((key, Value::Real(single)));
            if single != value {
                doubles.push((double_key, Value::Real(value)));
            }
        }
        fields.extend(doubles);
        let mut rgb: Vec<String> = components[..3]
            .iter()
            .map(|value| short_number(*value))
            .collect();
        if rgba[3] != 255 {
            rgb.push(short_number(components[3]));
        }
        fields.push(("NSRGB", Value::Data(rgb.join(" ").into_bytes())));
        self.object("UIColor", fields)
    }

    fn class(&mut self, name: &'static str) -> Value {
        if let Some(uid) = self.classes.get(name) {
            return uid.clone();
        }
        let mut class = Dictionary::new();
        class.insert("$classname".to_owned(), name.into());
        if let Some(chain) = class_chain(name) {
            class.insert(
                "$classes".to_owned(),
                Value::Array(chain.iter().map(|&name| name.into()).collect()),
            );
        }
        if name == "UIColor" {
            class.insert(
                "$classhints".to_owned(),
                Value::Array(vec!["NSColor".into()]),
            );
        }
        let uid = self.add(Value::Dictionary(class));
        self.classes.insert(name, uid.clone());
        uid
    }

    pub(super) fn finish(self, root: &Value) -> Result<Vec<u8>> {
        let mut top = Dictionary::new();
        top.insert(self.top_key.to_owned(), root.clone());
        let mut archive = Dictionary::new();
        archive.insert("$version".to_owned(), integer(100_000));
        archive.insert("$archiver".to_owned(), self.archiver.into());
        archive.insert("$top".to_owned(), Value::Dictionary(top));
        archive.insert("$objects".to_owned(), Value::Array(self.objects));
        binary_plist(Value::Dictionary(archive))
    }
}

/// The inheritance chains Notability registers; `InkedSpatialHash` has none.
fn class_chain(name: &str) -> Option<Vec<&str>> {
    Some(match name {
        "InkedSpatialHash" => return None,
        "NSMutableArray" => vec![name, "NSArray", "NSObject"],
        "NSMutableDictionary" => vec![name, "NSDictionary", "NSObject"],
        "NSMutableString" => vec![name, "NSString", "NSObject"],
        "NBReflowStateLocked" | "NBReflowStateReflowable" => {
            vec![name, "NBReflowState", "NSObject"]
        }
        "ImageMediaObject" | "CanvasMediaObject" => vec![name, "GenericMediaObject", "NSObject"],
        "TextBlockMediaObject" => vec![name, "CanvasMediaObject", "GenericMediaObject", "NSObject"],
        "ImageObject" => vec![name, "DrawObject", "NSObject"],
        "Notability.NoteDocumentPaperLayoutModel" => {
            vec!["NoteDocumentPaperLayoutModel", "NSObject"]
        }
        _ => vec![name, "NSObject"],
    })
}

pub(super) fn integer(value: i64) -> Value {
    Value::Integer(value.into())
}

pub(super) fn null() -> Value {
    Value::Uid(Uid::new(0))
}

pub(super) fn binary_plist(value: Value) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    value.to_writer_binary(&mut bytes)?;
    Ok(bytes)
}

/// Up to three significant digits without trailing zeros, as in `NSRGB` payloads.
fn short_number(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    let decimals = (2 - value.abs().log10().floor() as i32).max(0) as usize;
    let text = format!("{value:.decimals$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::short_number;

    #[test]
    fn formats_nsrgb_components_like_notability() {
        let rgb: Vec<String> = [18u8, 3, 119, 255, 244, 142, 0]
            .iter()
            .map(|value| short_number(f64::from(*value) / 255.0))
            .collect();
        assert_eq!(
            rgb,
            ["0.0706", "0.0118", "0.467", "1", "0.957", "0.557", "0"]
        );
    }
}
