use super::Object;
use crate::writer::Writer;
use crate::Result;
use std::io::Write;

#[derive(Debug, Clone)]
pub struct Operation {
    pub operator: String,
    pub operands: Vec<Object>,
}

impl Operation {
    pub fn new(operator: &str, operands: Vec<Object>) -> Operation {
        Operation {
            operator: operator.to_string(),
            operands,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Content<Operations: AsRef<[Operation]> = Vec<Operation>> {
    pub operations: Operations,
}

impl<Operations: AsRef<[Operation]>> Content<Operations> {
    /// Encode content operations.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut inner_buffer = vec![];
        let mut buffer = super::writer::CountingWrite {
            inner: &mut inner_buffer,
            bytes_written: 0
        };
        for operation in self.operations.as_ref() {
            for operand in &operation.operands {
                Writer::write_object(&mut buffer, operand, None, None)?;
                buffer.write_all(b" ")?;
            }
            buffer.write_all(operation.operator.as_bytes())?;
            buffer.write_all(b"\n")?;
        }
        Ok(inner_buffer)
    }
}
