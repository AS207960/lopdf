use std::fs::File;
use std::io::{BufWriter, Result, Write};
use std::path::Path;

use super::Object::*;
use super::{Dictionary, Document, Object, Stream, StringFormat};
use crate::xref::*;
use byteorder::{BigEndian, WriteBytesExt};

impl Document {
    /// Save PDF document to specified file path.
    #[inline]
    pub fn save<P: AsRef<Path>>(&mut self, path: P) -> Result<File> {
        let mut file = BufWriter::new(File::create(path)?);
        self.save_internal(&mut file)?;
        Ok(file.into_inner()?)
    }

    /// Save PDF to arbitrary target
    #[inline]
    pub fn save_to<W: Write>(&mut self, target: &mut W) -> Result<()> {
        self.save_internal(target)
    }

    fn save_internal<W: Write>(&mut self, target: &mut W) -> Result<()> {
        let mut target = CountingWrite {
            inner: target,
            bytes_written: 0,
        };

        let mut xref = Xref::new(self.max_id + 1);
        writeln!(target, "%PDF-{}", self.version)?;

        let mut contents_map = Some(std::collections::btree_map::BTreeMap::<crate::ObjectId, (u32, u32)>::new());

        for (&oid, object) in &self.objects {
            if object
                .type_name()
                .map(|name| ["ObjStm", "XRef", "Linearized"].contains(&name))
                .ok()
                != Some(true)
            {
                contents_map = Writer::write_indirect_object(&mut target, oid, object, &mut xref, contents_map)?;
            }
        }

        let xref_start = target.bytes_written;
        Writer::write_xref(&mut target, &xref)?;
        self.write_trailer(&mut target)?;
        write!(target, "\nstartxref\n{}\n%%EOF", xref_start)?;

        Ok(())
    }

    fn write_trailer<W: Write>(&mut self, file: &mut CountingWrite<&mut W>) -> Result<()> {
        self.trailer.set("Size", i64::from(self.max_id + 1));
        file.write_all(b"trailer\n")?;
        Writer::write_dictionary(file, &self.trailer, None, None)?;
        Ok(())
    }
}

pub struct Writer;

impl Writer {
    fn need_separator(object: &Object) -> bool {
        matches!(*object, Null | Boolean(_) | Integer(_) | Real(_) | Reference(_))
    }

    fn need_end_separator(object: &Object) -> bool {
        matches!(
            *object,
            Null | Boolean(_) | Integer(_) | Real(_) | Name(_) | Reference(_) | Object::Stream(_)
        )
    }

    pub fn write_xref(file: &mut dyn Write, xref: &Xref) -> Result<()> {
        let mut start = 0;
        let mut current = 1;
        let mut entries: Vec<Option<&super::xref::XrefEntry>> = vec![];

        writeln!(file, "xref")?;

        let mut output_entries = |start: u32, entries: &mut Vec<Option<&super::xref::XrefEntry>>| -> Result<()> {
            let len = if start == 0 {
                entries.len() + 1
            } else {
                entries.len()
            };
            writeln!(file, "{} {}", start, len)?;

            let mut write_xref_entry =
                |offset: u32, generation: u16, kind: char| writeln!(file, "{:>010} {:>05} {} ", offset, generation, kind);

            if start == 0 {
                write_xref_entry(0, 65535, 'f')?;
            }
            for entry in entries.drain(..) {
                if let Some(entry) = entry {
                    if let XrefEntry::Normal { offset, generation } = *entry {
                        write_xref_entry(offset, generation, 'n')?;
                    };
                } else {
                    write_xref_entry(0, 65535, 'f')?;
                }
            }
            Ok(())
        };

        let mut keys = xref.entries.keys().collect::<Vec<_>>();
        keys.sort();
        for oid in keys {
            if *oid != current {
                output_entries(start, &mut entries)?;
                start = *oid;
                current = *oid;
            }
            entries.push(xref.get(*oid));
            current += 1;
        }
        output_entries(start, &mut entries)?;
        Ok(())
    }

    pub fn write_xref_stream(xref: &Xref) -> (Vec<u8>, Vec<(i64, i64)>) {
        let mut start = 0;
        let mut current = 1;
        let mut entries: Vec<Option<&super::xref::XrefEntry>> = vec![];
        let mut out = vec![];
        let mut indices = vec![];

        let mut output_entries = |start: u32, entries: &mut Vec<Option<&super::xref::XrefEntry>>| {
            let len = if start == 0 {
                entries.len() + 1
            } else {
                entries.len()
            };
            indices.push((start as i64, len as i64));

            let mut write_xref_entry = |offset: u32, generation: u16, kind: u8| {
                out.write_u8(kind).unwrap();
                out.write_u32::<BigEndian>(offset).unwrap();
                out.write_u16::<BigEndian>(generation).unwrap();
            };

            if start == 0 {
                write_xref_entry(0, 65535, 0);
            }
            for entry in entries.drain(..) {
                if let Some(entry) = entry {
                    match entry {
                        XrefEntry::Normal { offset, generation } => {
                            write_xref_entry(*offset, *generation, 1);
                        }
                        XrefEntry::Compressed { container, index } => {
                            write_xref_entry(*container, *index, 2);
                        }
                        XrefEntry::Free => {}
                    }
                } else {
                    write_xref_entry(0, 65535, 0);
                }
            }
        };

        let mut keys = xref.entries.keys().collect::<Vec<_>>();
        keys.sort();
        for oid in keys {
            if *oid != current {
                output_entries(start, &mut entries);
                start = *oid;
                current = *oid;
            }
            entries.push(xref.get(*oid));
            current += 1;
        }
        output_entries(start, &mut entries);

        (out, indices)
    }

    pub fn write_indirect_object<W: Write>(
        file: &mut CountingWrite<&mut W>, oid: crate::ObjectId, object: &Object, xref: &mut Xref,
        contents_map: Option<std::collections::btree_map::BTreeMap<crate::ObjectId, (u32, u32)>>
    ) -> Result<Option<std::collections::btree_map::BTreeMap<crate::ObjectId, (u32, u32)>>> {
        let offset = file.bytes_written as u32;
        xref.insert(oid.0, XrefEntry::Normal { offset, generation: oid.1 });
        write!(
            file,
            "{} {} obj{}",
            oid.0,
            oid.1,
            if Writer::need_separator(object) { " " } else { "" }
        )?;
        let contents_map = Writer::write_object(file, object, Some(oid), contents_map)?;
        writeln!(
            file,
            "{}endobj",
            if Writer::need_end_separator(object) { " " } else { "" }
        )?;
        Ok(contents_map)
    }

    pub fn write_object<W: Write>(
        file: &mut CountingWrite<&mut W>, object: &Object, oid: Option<crate::ObjectId>,
        contents_map: Option<std::collections::btree_map::BTreeMap<crate::ObjectId, (u32, u32)>>
    ) -> Result<Option<std::collections::btree_map::BTreeMap<crate::ObjectId, (u32, u32)>>> {
        match *object {
            Null => {
                file.write_all(b"null")?;
                Ok(contents_map)
            },
            Boolean(ref value) => {
                if *value {
                    file.write_all(b"true")?;
                } else {
                    file.write_all(b"false")?;
                }
                Ok(contents_map)
            }
            Integer(ref value) => {
                let mut buf = itoa::Buffer::new();
                file.write_all(buf.format(*value).as_bytes())?;
                Ok(contents_map)
            }
            Real(ref value) => {
                write!(file, "{}", value)?;
                Ok(contents_map)
            },
            Name(ref name) => {
                Writer::write_name(file, name)?;
                Ok(contents_map)
            },
            String(ref text, ref format) => {
                Writer::write_string(file, text, format)?;
                Ok(contents_map)
            },
            Array(ref array) => Writer::write_array(file, array, oid, contents_map),
            Object::Dictionary(ref dict) => Writer::write_dictionary(file, dict, oid, contents_map),
            Object::Stream(ref stream) => Writer::write_stream(file, stream, oid, contents_map),
            Reference(ref id) => {
                write!(file, "{} {} R", id.0, id.1)?;
                Ok(contents_map)
            },
        }
    }

    fn write_name(file: &mut dyn Write, name: &[u8]) -> Result<()> {
        file.write_all(b"/")?;
        for &byte in name {
            // white-space and delimiter chars are encoded to # sequences
            // also encode bytes outside of the range 33 (!) to 126 (~)
            if b" \t\n\r\x0C()<>[]{}/%#".contains(&byte) || byte < 33 || byte > 126 {
                write!(file, "#{:02X}", byte)?;
            } else {
                file.write_all(&[byte])?;
            }
        }
        Ok(())
    }

    fn write_string(file: &mut dyn Write, text: &[u8], format: &StringFormat) -> Result<()> {
        match *format {
            // Within a Literal string, backslash (\) and unbalanced parentheses should be escaped.
            // This rule apply to each individual byte in a string object,
            // whether the string is interpreted as single-byte or multiple-byte character codes.
            // If an end-of-line marker appears within a literal string without a preceding backslash, the result is equivalent to \n.
            // So \r also need be escaped.
            StringFormat::Literal => {
                let mut escape_indice = Vec::new();
                let mut parentheses = Vec::new();
                for (index, &byte) in text.iter().enumerate() {
                    match byte {
                        b'(' => parentheses.push(index),
                        b')' => {
                            if !parentheses.is_empty() {
                                parentheses.pop();
                            } else {
                                escape_indice.push(index);
                            }
                        }
                        b'\\' | b'\r' => escape_indice.push(index),
                        _ => continue,
                    }
                }
                escape_indice.append(&mut parentheses);

                file.write_all(b"(")?;
                if !escape_indice.is_empty() {
                    for (index, &byte) in text.iter().enumerate() {
                        if escape_indice.contains(&index) {
                            file.write_all(b"\\")?;
                            file.write_all(&[if byte == b'\r' { b'r' } else { byte }])?;
                        } else {
                            file.write_all(&[byte])?;
                        }
                    }
                } else {
                    file.write_all(text)?;
                }
                file.write_all(b")")?;
            }
            StringFormat::Hexadecimal => {
                file.write_all(b"<")?;
                for &byte in text {
                    write!(file, "{:02X}", byte)?;
                }
                file.write_all(b">")?;
            }
        }
        Ok(())
    }

    pub fn write_array<W: Write>(
        file: &mut CountingWrite<&mut W>, array: &[Object], oid: Option<crate::ObjectId>,
        mut contents_map: Option<std::collections::btree_map::BTreeMap<crate::ObjectId, (u32, u32)>>
    ) -> Result<Option<std::collections::btree_map::BTreeMap<crate::ObjectId, (u32, u32)>>> {
        file.write_all(b"[")?;
        let mut first = true;
        for object in array {
            if first {
                first = false;
            } else if Writer::need_separator(object) {
                file.write_all(b" ")?;
            }
            contents_map = Writer::write_object(file, object, oid, contents_map)?;
        }
        file.write_all(b"]")?;
        Ok(contents_map)
    }

    pub fn write_dictionary<W: Write>(
        file: &mut CountingWrite<&mut W>, dictionary: &Dictionary, oid: Option<crate::ObjectId>,
        mut contents_map: Option<std::collections::btree_map::BTreeMap<crate::ObjectId, (u32, u32)>>
    ) -> Result<Option<std::collections::btree_map::BTreeMap<crate::ObjectId, (u32, u32)>>> {
        file.write_all(b"<<")?;
        for (key, value) in dictionary {
            Writer::write_name(file, key)?;
            if Writer::need_separator(value) {
                file.write_all(b" ")?;
            }
            let start = file.bytes_written as u32;
            contents_map = Writer::write_object(file, value, oid, contents_map)?;
            if key == b"Contents" {
                match (oid, &mut contents_map) {
                    (Some(oid), Some(ref mut contents_map)) => {
                        contents_map.insert(oid, (start, file.bytes_written as u32));
                    },
                    _ => {}
                }
            }
        }
        file.write_all(b">>")?;
        Ok(contents_map)
    }

    pub fn write_stream<W: Write>(
        file: &mut CountingWrite<&mut W>, stream: &Stream, oid: Option<crate::ObjectId>,
        mut contents_map: Option<std::collections::btree_map::BTreeMap<crate::ObjectId, (u32, u32)>>
    ) -> Result<Option<std::collections::btree_map::BTreeMap<crate::ObjectId, (u32, u32)>>> {
        contents_map = Writer::write_dictionary(file, &stream.dict, oid, contents_map)?;
        file.write_all(b"stream\n")?;
        file.write_all(&stream.content)?;
        file.write_all(b"endstream")?;
        Ok(contents_map)
    }
}

pub struct CountingWrite<W: Write> {
    pub inner: W,
    pub bytes_written: usize,
}

impl<W: Write> Write for CountingWrite<W> {
    #[inline]
    fn write(&mut self, buffer: &[u8]) -> Result<usize> {
        let result = self.inner.write(buffer);
        if let Ok(bytes) = result {
            self.bytes_written += bytes;
        }
        result
    }

    #[inline]
    fn write_all(&mut self, buffer: &[u8]) -> Result<()> {
        self.bytes_written += buffer.len();
        // If this returns `Err` we can’t know how many bytes were actually written (if any)
        // but that doesn’t matter since we’re gonna abort the entire PDF generation anyway.
        self.inner.write_all(buffer)
    }

    #[inline]
    fn flush(&mut self) -> Result<()> {
        self.inner.flush()
    }
}

#[test]
fn save_document() {
    let mut doc = Document::with_version("1.5");
    doc.objects.insert((1, 0), Null);
    doc.objects.insert((2, 0), Boolean(true));
    doc.objects.insert((3, 0), Integer(3));
    doc.objects.insert((4, 0), Real(0.5));
    doc.objects
        .insert((5, 0), String("text((\r)".as_bytes().to_vec(), StringFormat::Literal));
    doc.objects.insert(
        (6, 0),
        String("text((\r)".as_bytes().to_vec(), StringFormat::Hexadecimal),
    );
    doc.objects.insert((7, 0), Name(b"name \t".to_vec()));
    doc.objects.insert((8, 0), Reference((1, 0)));
    doc.objects
        .insert((9, 2), Array(vec![Integer(1), Integer(2), Integer(3)]));
    doc.objects
        .insert((11, 0), Stream(Stream::new(Dictionary::new(), vec![0x41, 0x42, 0x43])));
    let mut dict = Dictionary::new();
    dict.set("A", Null);
    dict.set("B", false);
    dict.set("C", Name(b"name".to_vec()));
    doc.objects.insert((12, 0), Object::Dictionary(dict));
    doc.max_id = 12;

    doc.save("test_0_save.pdf").unwrap();
}
