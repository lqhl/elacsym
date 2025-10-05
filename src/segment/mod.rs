//! Segment management
//!
//! Segments store document data in columnar Parquet format

use arrow::array::{
    Array, ArrayRef, BooleanArray, BooleanBuilder, FixedSizeListArray, Float32Builder,
    Float64Array, Float64Builder, GenericListArray, Int64Array, Int64Builder, RecordBatch,
    StringArray, StringBuilder, UInt64Array, UInt64Builder,
};
use arrow::datatypes::{DataType, Field, Schema as ArrowSchema};
use bytes::Bytes;
use parquet::arrow::{arrow_reader::ParquetRecordBatchReaderBuilder, ArrowWriter};
use parquet::file::properties::WriterProperties;
use std::collections::HashMap;
use std::sync::Arc;

use crate::types::{AttributeType, AttributeValue, Document, Schema};
use crate::{Error, Result};

/// Segment writer for creating Parquet files
pub struct SegmentWriter {
    schema: Schema,
    pub arrow_schema: Arc<ArrowSchema>,
}

impl SegmentWriter {
    pub fn new(schema: Schema) -> Result<Self> {
        let arrow_schema = Self::create_arrow_schema(&schema)?;
        Ok(Self {
            schema,
            arrow_schema,
        })
    }

    /// Create Arrow schema from elacsym schema
    fn create_arrow_schema(schema: &Schema) -> Result<Arc<ArrowSchema>> {
        let mut fields = vec![
            Field::new("id", DataType::UInt64, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, false)),
                    schema.vector_dim as i32,
                ),
                true, // nullable for updates without vector
            ),
        ];

        for (name, attr_schema) in &schema.attributes {
            let data_type = match attr_schema.attr_type {
                AttributeType::String => DataType::Utf8,
                AttributeType::Integer => DataType::Int64,
                AttributeType::Float => DataType::Float64,
                AttributeType::Boolean => DataType::Boolean,
                AttributeType::StringArray => {
                    DataType::List(Arc::new(Field::new("item", DataType::Utf8, true)))
                }
            };
            fields.push(Field::new(name, data_type, true));
        }

        Ok(Arc::new(ArrowSchema::new(fields)))
    }

    /// Write documents to Parquet format
    pub fn write_parquet(&self, documents: &[Document]) -> Result<Bytes> {
        let batch = self.documents_to_record_batch(documents)?;

        let mut buffer = Vec::new();
        let props = WriterProperties::builder()
            .set_compression(parquet::basic::Compression::SNAPPY)
            .build();

        let mut writer = ArrowWriter::try_new(&mut buffer, self.arrow_schema.clone(), Some(props))?;

        writer.write(&batch)?;
        writer.close()?;

        Ok(Bytes::from(buffer))
    }

    /// Convert documents to Arrow RecordBatch
    fn documents_to_record_batch(&self, documents: &[Document]) -> Result<RecordBatch> {
        let num_rows = documents.len();

        // Build ID column
        let mut id_builder = UInt64Builder::with_capacity(num_rows);
        for doc in documents {
            id_builder.append_value(doc.id);
        }
        let id_array = Arc::new(id_builder.finish());

        // Build Vector column (FixedSizeList)
        let mut vector_builder = Float32Builder::with_capacity(num_rows * self.schema.vector_dim);
        let mut list_offsets = vec![0i32];

        for doc in documents {
            if let Some(ref vector) = doc.vector {
                if vector.len() != self.schema.vector_dim {
                    return Err(Error::InvalidSchema(format!(
                        "Vector dimension mismatch: expected {}, got {}",
                        self.schema.vector_dim,
                        vector.len()
                    )));
                }
                for &v in vector {
                    vector_builder.append_value(v);
                }
            } else {
                // Append nulls for missing vectors
                for _ in 0..self.schema.vector_dim {
                    vector_builder.append_value(0.0); // Placeholder
                }
            }
            list_offsets.push(list_offsets.last().unwrap() + self.schema.vector_dim as i32);
        }

        let vector_values = Arc::new(vector_builder.finish());
        let vector_field = Arc::new(Field::new("item", DataType::Float32, false));
        let vector_array = Arc::new(FixedSizeListArray::new(
            vector_field,
            self.schema.vector_dim as i32,
            vector_values,
            None, // No nulls for now - we filled with 0s
        ));

        // Build attribute columns
        let mut columns: Vec<ArrayRef> = vec![id_array, vector_array];

        for (attr_name, attr_schema) in &self.schema.attributes {
            let array: ArrayRef = match attr_schema.attr_type {
                AttributeType::String => {
                    let mut builder = StringBuilder::with_capacity(num_rows, num_rows * 10);
                    for doc in documents {
                        if let Some(AttributeValue::String(s)) = doc.attributes.get(attr_name) {
                            builder.append_value(s);
                        } else {
                            builder.append_null();
                        }
                    }
                    Arc::new(builder.finish())
                }
                AttributeType::Integer => {
                    let mut builder = Int64Builder::with_capacity(num_rows);
                    for doc in documents {
                        if let Some(AttributeValue::Integer(i)) = doc.attributes.get(attr_name) {
                            builder.append_value(*i);
                        } else {
                            builder.append_null();
                        }
                    }
                    Arc::new(builder.finish())
                }
                AttributeType::Float => {
                    let mut builder = Float64Builder::with_capacity(num_rows);
                    for doc in documents {
                        if let Some(AttributeValue::Float(f)) = doc.attributes.get(attr_name) {
                            builder.append_value(*f);
                        } else {
                            builder.append_null();
                        }
                    }
                    Arc::new(builder.finish())
                }
                AttributeType::Boolean => {
                    let mut builder = BooleanBuilder::with_capacity(num_rows);
                    for doc in documents {
                        if let Some(AttributeValue::Boolean(b)) = doc.attributes.get(attr_name) {
                            builder.append_value(*b);
                        } else {
                            builder.append_null();
                        }
                    }
                    Arc::new(builder.finish())
                }
                AttributeType::StringArray => {
                    let mut builder = arrow::array::ListBuilder::new(StringBuilder::new());
                    for doc in documents {
                        if let Some(AttributeValue::StringArray(arr)) =
                            doc.attributes.get(attr_name)
                        {
                            for s in arr {
                                builder.values().append_value(s);
                            }
                            builder.append(true);
                        } else {
                            builder.append(false);
                        }
                    }
                    Arc::new(builder.finish())
                }
            };
            columns.push(array);
        }

        RecordBatch::try_new(self.arrow_schema.clone(), columns)
            .map_err(|e| Error::internal(format!("Failed to create RecordBatch: {}", e)))
    }
}

/// Segment reader for reading Parquet files
pub struct SegmentReader {
    _schema: Arc<ArrowSchema>,
}

impl SegmentReader {
    pub fn new(schema: Arc<ArrowSchema>) -> Self {
        Self { _schema: schema }
    }

    /// Read specific documents by IDs from Parquet bytes
    pub fn read_documents_by_ids(&self, data: Bytes, doc_ids: &[u64]) -> Result<Vec<Document>> {
        let all_docs = self.read_parquet(data)?;

        // Filter by requested IDs
        let id_set: std::collections::HashSet<u64> = doc_ids.iter().copied().collect();
        let filtered_docs: Vec<Document> = all_docs
            .into_iter()
            .filter(|doc| id_set.contains(&doc.id))
            .collect();

        Ok(filtered_docs)
    }

    /// Read documents from Parquet bytes
    pub fn read_parquet(&self, data: Bytes) -> Result<Vec<Document>> {
        let builder = ParquetRecordBatchReaderBuilder::try_new(data)
            .map_err(|e| Error::internal(format!("Failed to create Parquet reader: {}", e)))?;

        let reader = builder
            .build()
            .map_err(|e| Error::internal(format!("Failed to build Parquet reader: {}", e)))?;

        let mut documents = Vec::new();

        for batch_result in reader {
            let batch = batch_result
                .map_err(|e| Error::internal(format!("Failed to read batch: {}", e)))?;

            let batch_docs = self.record_batch_to_documents(batch)?;
            documents.extend(batch_docs);
        }

        Ok(documents)
    }

    /// Convert Arrow RecordBatch to documents
    fn record_batch_to_documents(&self, batch: RecordBatch) -> Result<Vec<Document>> {
        let num_rows = batch.num_rows();
        let schema = batch.schema();

        // Extract ID column
        let id_array = batch
            .column(0)
            .as_any()
            .downcast_ref::<UInt64Array>()
            .ok_or_else(|| Error::internal("ID column is not UInt64Array"))?;

        // Extract Vector column
        let vector_array = batch.column(1);

        let mut documents = Vec::with_capacity(num_rows);

        for row_idx in 0..num_rows {
            let id = id_array.value(row_idx);

            // Extract vector
            let vector = if vector_array.is_null(row_idx) {
                None
            } else {
                let fixed_list = vector_array
                    .as_any()
                    .downcast_ref::<FixedSizeListArray>()
                    .ok_or_else(|| Error::internal("Vector column is not FixedSizeListArray"))?;

                let values = fixed_list.value(row_idx);
                let float_array = values
                    .as_any()
                    .downcast_ref::<arrow::array::Float32Array>()
                    .ok_or_else(|| Error::internal("Vector values are not Float32Array"))?;

                let vec: Vec<f32> = (0..float_array.len())
                    .map(|i| float_array.value(i))
                    .collect();

                Some(vec)
            };

            // Extract attributes
            let mut attributes = HashMap::new();

            for col_idx in 2..batch.num_columns() {
                let field = schema.field(col_idx);
                let array = batch.column(col_idx);

                if array.is_null(row_idx) {
                    continue;
                }

                let value = match field.data_type() {
                    DataType::Utf8 => {
                        let string_array = array
                            .as_any()
                            .downcast_ref::<StringArray>()
                            .ok_or_else(|| Error::internal("String column type mismatch"))?;
                        AttributeValue::String(string_array.value(row_idx).to_string())
                    }
                    DataType::Int64 => {
                        let int_array = array
                            .as_any()
                            .downcast_ref::<Int64Array>()
                            .ok_or_else(|| Error::internal("Int64 column type mismatch"))?;
                        AttributeValue::Integer(int_array.value(row_idx))
                    }
                    DataType::Float64 => {
                        let float_array = array
                            .as_any()
                            .downcast_ref::<Float64Array>()
                            .ok_or_else(|| Error::internal("Float64 column type mismatch"))?;
                        AttributeValue::Float(float_array.value(row_idx))
                    }
                    DataType::Boolean => {
                        let bool_array = array
                            .as_any()
                            .downcast_ref::<BooleanArray>()
                            .ok_or_else(|| Error::internal("Boolean column type mismatch"))?;
                        AttributeValue::Boolean(bool_array.value(row_idx))
                    }
                    DataType::List(_) => {
                        let list_array = array
                            .as_any()
                            .downcast_ref::<GenericListArray<i32>>()
                            .ok_or_else(|| Error::internal("List column type mismatch"))?;

                        let list_value = list_array.value(row_idx);
                        let string_array = list_value
                            .as_any()
                            .downcast_ref::<StringArray>()
                            .ok_or_else(|| Error::internal("List values are not strings"))?;

                        let string_vec: Vec<String> = (0..string_array.len())
                            .map(|i| string_array.value(i).to_string())
                            .collect();

                        AttributeValue::StringArray(string_vec)
                    }
                    _ => {
                        return Err(Error::internal(format!(
                            "Unsupported data type: {:?}",
                            field.data_type()
                        )))
                    }
                };

                attributes.insert(field.name().clone(), value);
            }

            documents.push(Document {
                id,
                vector,
                attributes,
            });
        }

        Ok(documents)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AttributeSchema, DistanceMetric, FullTextConfig};

    #[test]
    fn test_parquet_roundtrip() {
        let mut schema_attrs = HashMap::new();
        schema_attrs.insert(
            "title".to_string(),
            AttributeSchema {
                attr_type: AttributeType::String,
                indexed: false,
                full_text: FullTextConfig::Simple(true),
            },
        );
        schema_attrs.insert(
            "score".to_string(),
            AttributeSchema {
                attr_type: AttributeType::Float,
                indexed: false,
                full_text: FullTextConfig::Simple(false),
            },
        );

        let schema = Schema {
            vector_dim: 3,
            vector_metric: DistanceMetric::Cosine,
            attributes: schema_attrs,
        };

        // Create test documents
        let mut doc1_attrs = HashMap::new();
        doc1_attrs.insert(
            "title".to_string(),
            AttributeValue::String("Test Doc".to_string()),
        );
        doc1_attrs.insert("score".to_string(), AttributeValue::Float(4.5));

        let doc1 = Document {
            id: 1,
            vector: Some(vec![0.1, 0.2, 0.3]),
            attributes: doc1_attrs,
        };

        let documents = vec![doc1];

        // Write to Parquet
        let writer = SegmentWriter::new(schema.clone()).unwrap();
        let parquet_data = writer.write_parquet(&documents).unwrap();

        // Read back from Parquet
        let reader = SegmentReader::new(writer.arrow_schema.clone());
        let read_docs = reader.read_parquet(parquet_data).unwrap();

        assert_eq!(read_docs.len(), 1);
        assert_eq!(read_docs[0].id, 1);
        assert_eq!(read_docs[0].vector, Some(vec![0.1, 0.2, 0.3]));

        let title = read_docs[0].attributes.get("title").and_then(|v| match v {
            AttributeValue::String(s) => Some(s.as_str()),
            _ => None,
        });
        assert_eq!(title, Some("Test Doc"));

        let score = read_docs[0].attributes.get("score").and_then(|v| match v {
            AttributeValue::Float(f) => Some(*f),
            _ => None,
        });
        assert_eq!(score, Some(4.5));
    }
}
