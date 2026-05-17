use crate::errors::StorageResult;

pub enum RowValue {
    Null,
    Bool(bool),
    Int64(i64),
    Float64(f64),
    String(String),
    Bytes(Vec<u8>),
}

impl RowValue {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            RowValue::Null => Vec::with_capacity(0),
            RowValue::Bool(b) => vec![if *b { 1 } else { 0 }],
            RowValue::Int64(i) => {
                let mut encoded = Vec::with_capacity(8);
                encoded.extend_from_slice(&i.to_be_bytes());
                encoded
            }
            RowValue::Float64(f) => {
                let mut encoded = Vec::with_capacity(8);
                encoded.extend_from_slice(&f.to_be_bytes());
                encoded
            }
            RowValue::String(s) => {
                let mut encoded = Vec::with_capacity(s.len());
                encoded.extend_from_slice(s.as_bytes());
                encoded
            }
            RowValue::Bytes(b) => {
                let mut encoded = Vec::with_capacity(b.len());
                encoded.extend_from_slice(b);
                encoded
            }
        }
    }

    pub fn decode(data: &[u8], tp: RowValue) -> StorageResult<RowValue> {
        let res = match tp {
            RowValue::Null => RowValue::Null,
            RowValue::Bool(_) => RowValue::Bool(data[0] == 1),
            RowValue::Int64(_) => {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&data[..8]);
                RowValue::Int64(i64::from_be_bytes(bytes))
            }
            RowValue::Float64(_) => {
                let mut bytes = [0u8; 8];
                bytes.copy_from_slice(&data[..8]);
                RowValue::Float64(f64::from_be_bytes(bytes))
            }
            RowValue::String(_) => RowValue::String(String::from_utf8_lossy(data).into_owned()),
            RowValue::Bytes(_) => RowValue::Bytes(data.to_vec()),
        };
        Ok(res)
    }
}

// encode format: format version(1 bytes) + null_bitmap((column num + 7) / 8 bytes) + fixed area (n bytes) + varlen slot (varlen column num * 2 bytes) + varlen data (sum of varlen column length bytes)

pub struct RowData {
    pub values: Vec<RowValue>,
}
