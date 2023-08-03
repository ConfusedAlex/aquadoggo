// SPDX-License-Identifier: AGPL-3.0-or-later

use std::num::NonZeroU64;

use p2panda_rs::document::traits::AsDocument;
use p2panda_rs::document::{DocumentId, DocumentViewId};
use p2panda_rs::operation::OperationValue;
use p2panda_rs::schema::{Schema, SchemaId};
use p2panda_rs::storage_provider::traits::DocumentStore;

use crate::db::errors::BlobStoreError;
use crate::db::query::{Field, Filter, Order, Pagination, Select};
use crate::db::stores::query::{Query, RelationList};
use crate::db::SqlStore;

/// The maximum allowed number of blob pieces per blob.
/// @TODO: do we want this? If so, what value should it be and we should add this to
/// p2panda-rs blob validation too.
const MAX_BLOB_PIECES: u64 = 10000;

pub type BlobData = String;

impl SqlStore {
    /// Get the data for one blob from the store, identified by it's document id.
    pub async fn get_blob(&self, id: &DocumentId) -> Result<Option<BlobData>, BlobStoreError> {
        // Get the root blob document.
        let blob_document = match self.get_document(id).await? {
            Some(document) => {
                if document.schema_id != SchemaId::Blob(1) {
                    return Err(BlobStoreError::NotBlobDocument);
                }
                document
            }
            None => return Ok(None),
        };
        document_to_blob_data(self, blob_document).await
    }

    /// Get the data for one blob from the store, identified by it's document view id.
    pub async fn get_blob_by_view_id(
        &self,
        view_id: &DocumentViewId,
    ) -> Result<Option<BlobData>, BlobStoreError> {
        // Get the root blob document.
        let blob_document = match self.get_document_by_view_id(view_id).await? {
            Some(document) => {
                if document.schema_id != SchemaId::Blob(1) {
                    return Err(BlobStoreError::NotBlobDocument);
                }
                document
            }
            None => return Ok(None),
        };
        document_to_blob_data(self, blob_document).await
    }
}

/// Helper method for validation and parsing a document into blob data.
async fn document_to_blob_data(
    store: &SqlStore,
    blob: impl AsDocument,
) -> Result<Option<BlobData>, BlobStoreError> {
    // Get the length of the blob.
    let length = match blob.get("length").unwrap() {
        OperationValue::Integer(length) => length,
        _ => panic!(), // We should never hit this as we already validated that this is a blob document.
    };

    // Get the number of pieces in the blob.
    let num_pieces = match blob.get("pieces").unwrap() {
        OperationValue::PinnedRelationList(list) => list.len(),
        _ => panic!(), // We should never hit this as we already validated that this is a blob document.
    };

    // Now collect all exiting pieces for the blob.
    //
    // We do this using the stores' query method, targeting pieces which are in the relation
    // list of the blob.
    let schema = Schema::get_system(SchemaId::BlobPiece(1)).unwrap();
    let list = RelationList::new_pinned(blob.view_id(), "pieces");
    let pagination = Pagination {
        first: NonZeroU64::new(MAX_BLOB_PIECES).unwrap(),
        ..Default::default()
    };

    let args = Query::new(
        &pagination,
        &Select::new(&[Field::new("data")]),
        &Filter::default(),
        &Order::default(),
    );

    let (_, results) = store.query(schema, &args, Some(&list)).await?;

    // No pieces were found.
    if results.is_empty() {
        return Err(BlobStoreError::NoBlobPiecesFound);
    };

    // Not all pieces were found.
    if results.len() != num_pieces {
        return Err(BlobStoreError::MissingPieces);
    }

    // Now we construct the blob data.
    let mut blob_data = "".to_string();

    for (_, blob_piece_document) in results {
        match blob_piece_document
            .get("data")
            .expect("Blob piece document without \"data\" field")
        {
            OperationValue::String(data_str) => blob_data += data_str,
            _ => panic!(), // We should never hit this as we only queried for blob piece documents.
        }
    }

    // Combined blob data length doesn't match the claimed length.
    if blob_data.len() != *length as usize {
        return Err(BlobStoreError::IncorrectLength);
    };

    Ok(Some(blob_data))
}

#[cfg(test)]
mod tests {
    use p2panda_rs::document::DocumentId;
    use p2panda_rs::identity::KeyPair;
    use p2panda_rs::schema::SchemaId;
    use p2panda_rs::test_utils::fixtures::{key_pair, random_document_view_id};
    use rstest::rstest;

    use crate::db::errors::BlobStoreError;
    use crate::test_utils::{add_document, test_runner, TestNode};

    #[rstest]
    fn get_blob(key_pair: KeyPair) {
        test_runner(|mut node: TestNode| async move {
            let blob_data = "Hello, World!".to_string();

            // Publish blob pieces and blob.
            let blob_piece_view_id_1 = add_document(
                &mut node,
                &SchemaId::BlobPiece(1),
                vec![("data", blob_data[..5].into())],
                &key_pair,
            )
            .await;

            let blob_piece_view_id_2 = add_document(
                &mut node,
                &SchemaId::BlobPiece(1),
                vec![("data", blob_data[5..].into())],
                &key_pair,
            )
            .await;
            let blob_view_id = add_document(
                &mut node,
                &SchemaId::Blob(1),
                vec![
                    ("length", { blob_data.len() as i64 }.into()),
                    ("mime_type", "text/plain".into()),
                    (
                        "pieces",
                        vec![blob_piece_view_id_1, blob_piece_view_id_2].into(),
                    ),
                ],
                &key_pair,
            )
            .await;

            let document_id: DocumentId = blob_view_id.to_string().parse().unwrap();

            // Get blob by document id.
            let blob = node.context.store.get_blob(&document_id).await.unwrap();

            assert!(blob.is_some());
            assert_eq!(blob.unwrap(), blob_data);

            // Get blob by view id.
            let blob = node
                .context
                .store
                .get_blob_by_view_id(&blob_view_id)
                .await
                .unwrap();

            assert!(blob.is_some());
            assert_eq!(blob.unwrap(), blob_data)
        })
    }

    #[rstest]
    fn get_blob_errors(key_pair: KeyPair) {
        test_runner(|mut node: TestNode| async move {
            let blob_data = "Hello, World!".to_string();

            // Publish a blob containing pieces which aren't in the store.
            let blob_view_id = add_document(
                &mut node,
                &SchemaId::Blob(1),
                vec![
                    ("length", { blob_data.len() as i64 }.into()),
                    ("mime_type", "text/plain".into()),
                    (
                        "pieces",
                        vec![random_document_view_id(), random_document_view_id()].into(),
                    ),
                ],
                &key_pair,
            )
            .await;

            let blob_document_id: DocumentId = blob_view_id.to_string().parse().unwrap();

            // We get the correct `NoBlobPiecesFound` error.
            let result = node.context.store.get_blob(&blob_document_id).await;
            assert!(
                matches!(result, Err(BlobStoreError::NoBlobPiecesFound)),
                "{:?}",
                result
            );

            // Publish one blob piece.
            let blob_piece_view_id_1 = add_document(
                &mut node,
                &SchemaId::BlobPiece(1),
                vec![("data", blob_data[..5].into())],
                &key_pair,
            )
            .await;

            // Publish a blob with one piece that is in the store and one that isn't.
            let blob_view_id = add_document(
                &mut node,
                &SchemaId::Blob(1),
                vec![
                    ("length", { blob_data.len() as i64 }.into()),
                    ("mime_type", "text/plain".into()),
                    (
                        "pieces",
                        vec![blob_piece_view_id_1.clone(), random_document_view_id()].into(),
                    ),
                ],
                &key_pair,
            )
            .await;

            let blob_document_id: DocumentId = blob_view_id.to_string().parse().unwrap();

            // We should get the correct `MissingBlobPieces` error.
            let result = node.context.store.get_blob(&blob_document_id).await;
            assert!(
                matches!(result, Err(BlobStoreError::MissingPieces)),
                "{:?}",
                result
            );

            // Publish one more blob piece, but it doesn't contain the correct number of bytes.
            let blob_piece_view_id_2 = add_document(
                &mut node,
                &SchemaId::BlobPiece(1),
                vec![("data", blob_data[9..].into())],
                &key_pair,
            )
            .await;

            // Publish a blob with two pieces that are in the store but they don't add up to the
            // right byte length.
            let blob_view_id = add_document(
                &mut node,
                &SchemaId::Blob(1),
                vec![
                    ("length", { blob_data.len() as i64 }.into()),
                    ("mime_type", "text/plain".into()),
                    (
                        "pieces",
                        vec![blob_piece_view_id_1, blob_piece_view_id_2].into(),
                    ),
                ],
                &key_pair,
            )
            .await;

            let blob_document_id: DocumentId = blob_view_id.to_string().parse().unwrap();

            // We get the correct `IncorrectLength` error.
            let result = node.context.store.get_blob(&blob_document_id).await;
            assert!(
                matches!(result, Err(BlobStoreError::IncorrectLength)),
                "{:?}",
                result
            );
        })
    }
}
