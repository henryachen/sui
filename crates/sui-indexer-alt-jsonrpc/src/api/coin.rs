// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use anyhow::Context as _;

use super::rpc_module::RpcModule;
use crate::context::Context;
use crate::data::objects::fetch_latest;
use crate::error::{invalid_params, InternalContext, RpcError};
use crate::paginate::{Cursor, Page};
use diesel::prelude::*;
use jsonrpsee::{core::RpcResult, proc_macros::rpc};
use move_core_types::language_storage::TypeTag;
use serde::{Deserialize, Serialize};
use sui_indexer_alt_schema::objects::StoredObject;
use sui_indexer_alt_schema::schema::coin_balance_buckets;
use sui_json_rpc_types::{Coin, Page as JsonRpcPage};
use sui_open_rpc::Module;
use sui_open_rpc_macros::open_rpc;
use sui_types::{
    base_types::{ObjectID, SuiAddress},
    gas_coin::GAS,
    object::Object,
};

#[open_rpc(namespace = "suix", tag = "Coin API")]
#[rpc(server, namespace = "suix")]
trait CoinApi {
    #[method(name = "getCoins")]
    async fn get_coins(
        &self,
        owner: SuiAddress,
        coin_type: Option<String>,
        cursor: Option<String>,
        limit: Option<usize>,
    ) -> RpcResult<JsonRpcPage<Coin, String>>;

    /// Return all Coin objects owned by an address.
    #[method(name = "getAllCoins")]
    async fn get_all_coins(
        &self,
        /// the owner's Sui address
        owner: SuiAddress,
        /// optional paging cursor
        cursor: Option<String>,
        /// maximum number of items per page
        limit: Option<usize>,
    ) -> RpcResult<JsonRpcPage<Coin, String>>;
}

pub(crate) struct CoinServer(pub Context, pub CoinConfig);

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CoinConfig {
    /// The default page size limit when querying coins, if none is provided.
    pub default_page_size: usize,

    /// The largest acceptable page size when querying coins. Requesting a page larger than
    /// this is a user error.
    pub max_page_size: usize,
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum Error {
    #[error("Pagination issue: {0}")]
    Pagination(#[from] crate::paginate::Error),

    #[error("Error resolving type information: {0}")]
    Resolution(anyhow::Error),

    #[error("Deserialization error: {0}")]
    Deserialization(#[from] bcs::Error),
}

#[derive(Queryable, Selectable, Debug, Serialize, Deserialize)]
#[diesel(table_name = coin_balance_buckets)]
struct IdCpBalance {
    object_id: Vec<u8>,
    #[allow(unused)]
    cp_sequence_number: i64,
    coin_balance_bucket: Option<i16>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct BalanceCursor {
    object_id: ObjectID,
    cp_sequence_number: i64,
    coin_balance_bucket: i16,
}

#[async_trait::async_trait]
impl CoinApiServer for CoinServer {
    async fn get_coins(
        &self,
        owner: SuiAddress,
        coin_type: Option<String>,
        cursor: Option<String>,
        limit: Option<usize>,
    ) -> RpcResult<JsonRpcPage<Coin, String>> {
        let coin_struct_tag = if let Some(coin_type) = coin_type {
            sui_types::parse_sui_type_tag(&coin_type)
                .map_err(|e| invalid_params(Error::Resolution(e)))?
        } else {
            GAS::type_tag()
        };
        Ok(self
            .get_coins_impl(owner, Some(coin_struct_tag), cursor, limit)
            .await
            .internal_context("Failed to get coins")?)
    }

    async fn get_all_coins(
        &self,
        owner: SuiAddress,
        cursor: Option<String>,
        limit: Option<usize>,
    ) -> RpcResult<JsonRpcPage<Coin, String>> {
        Ok(self
            .get_coins_impl(owner, None, cursor, limit)
            .await
            .internal_context("Failed to get coins")?)
    }
}

impl CoinServer {
    async fn get_coins_impl(
        &self,
        owner: SuiAddress,
        coin_type_tag: Option<TypeTag>,
        cursor: Option<String>,
        limit: Option<usize>,
    ) -> Result<JsonRpcPage<Coin, String>, RpcError<Error>> {
        let Self(ctx, config) = self;

        let page: Page<BalanceCursor> = Page::from_params(
            config.default_page_size,
            config.max_page_size,
            cursor,
            limit,
            None,
        )?;

        // We get all the qualified coin ids first.
        let coin_id_page = self.get_coin_id_page(owner, coin_type_tag, page).await?;

        // Then we load the actual objects from kv, to render the coins.
        let objects = fetch_latest(ctx.loader(), &coin_id_page.data)
            .await
            .context("Failed to load latest coins")?;

        // The resulting objects are no longer sorted according to original order so we sort them to match coin_ids order.
        let sorted_objects = coin_id_page
            .data
            .iter()
            .filter_map(|key| objects.get(key).cloned())
            .collect::<Vec<_>>();

        // Finally convert objects to coins.
        let coins: Vec<Coin> = sorted_objects
            .into_iter()
            .map(|o| try_convert_object_into_coin(o))
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to convert object to coin")?;

        Ok(JsonRpcPage {
            data: coins,
            next_cursor: coin_id_page.next_cursor,
            has_next_page: coin_id_page.has_next_page,
        })
    }

    async fn get_coin_id_page(
        &self,
        owner: SuiAddress,
        coin_type_tag: Option<TypeTag>,
        page: Page<BalanceCursor>,
    ) -> Result<JsonRpcPage<ObjectID, String>, RpcError<Error>> {
        use coin_balance_buckets::dsl as cb;

        let Self(ctx, _) = self;
        let mut conn = ctx
            .reader()
            .connect()
            .await
            .context("Failed to connect to database")?;

        let limit = page.limit;

        // We use two aliases of coin_balance_buckets to make the query more readable.
        let (candidates, newer) = diesel::alias!(
            coin_balance_buckets as candidates,
            coin_balance_buckets as newer
        );

        // Macros to make the query more readable.
        macro_rules! candidates {
            ($($field:ident),*) => {
                candidates.fields(($(cb::$field),*))
            }
        }

        macro_rules! newer {
            ($($field:ident),*) => {
                newer.fields(($(cb::$field),*))
            }
        }

        // Construct the basic query first to filter by owner, not deleted and newest rows.
        let mut query = candidates
            .select(candidates!(
                object_id,
                cp_sequence_number,
                coin_balance_bucket
            ))
            .left_join(
                newer.on(candidates!(object_id)
                    .eq(newer!(object_id))
                    .and(candidates!(cp_sequence_number).lt(newer!(cp_sequence_number)))),
            )
            .filter(newer!(object_id).is_null())
            .filter(candidates!(owner_id).eq(owner.to_vec()))
            .filter(candidates!(coin_balance_bucket).is_not_null())
            .into_boxed();

        // If the coin type is specified, we filter by it.
        if let Some(coin_type_tag) = coin_type_tag {
            let serialized_coin_type =
                bcs::to_bytes(&coin_type_tag).context("Failed to serialize coin type tag")?;
            query = query.filter(candidates!(coin_type).eq(serialized_coin_type));
        }

        // If the cursor is specified, we filter by it.
        if let Some(Cursor(BalanceCursor {
            object_id,
            cp_sequence_number,
            coin_balance_bucket,
        })) = page.cursor
        {
            query = query
                .filter(candidates!(cp_sequence_number).le(cp_sequence_number as i64))
                .filter(
                    // Since the result is ordered by coin_balance_bucket followed by object_id,
                    // we need to filter by coin_balance_bucket first, then if balance bucket is the same, object_id.
                    candidates!(coin_balance_bucket)
                        .lt(coin_balance_bucket)
                        .or(candidates!(coin_balance_bucket)
                            .eq(coin_balance_bucket)
                            .and(candidates!(object_id).gt(object_id.to_vec()))),
                );
        }

        // Finally we order by coin_balance_bucket and break ties by object_id.
        query = query
            .order_by(candidates!(coin_balance_bucket).desc())
            .then_order_by(candidates!(object_id).asc());

        let mut buckets: Vec<IdCpBalance> =
            conn.results(query).await.context("Failed to query coins")?;

        // Now gather pagination info.
        let has_next_page = buckets.len() > limit as usize;
        if has_next_page {
            buckets.truncate(limit as usize);
        }

        let next_cursor = buckets
            .last()
            .map(|bucket| {
                let object_id =
                    ObjectID::from_bytes(&bucket.object_id).context("Failed to parse object id")?;
                let balance_bucket = bucket
                    .coin_balance_bucket
                    .ok_or_else(|| anyhow::anyhow!("Coin balance bucket is null"))?;
                Cursor(BalanceCursor {
                    object_id,
                    cp_sequence_number: bucket.cp_sequence_number,
                    coin_balance_bucket: balance_bucket,
                })
                .encode()
                .context("Failed to encode cursor")
            })
            .transpose()?;

        let ids = buckets
            .iter()
            .map(|bucket| ObjectID::from_bytes(&bucket.object_id))
            .collect::<Result<Vec<_>, _>>()
            .context("Failed to parse object id")?;

        Ok(JsonRpcPage {
            data: ids,
            next_cursor,
            has_next_page,
        })
    }
}

impl RpcModule for CoinServer {
    fn schema(&self) -> Module {
        CoinApiOpenRpc::module_doc()
    }

    fn into_impl(self) -> jsonrpsee::RpcModule<Self> {
        self.into_rpc()
    }
}

impl Default for CoinConfig {
    fn default() -> Self {
        Self {
            default_page_size: 50,
            max_page_size: 100,
        }
    }
}

fn try_convert_object_into_coin(object: StoredObject) -> Result<Coin, Error> {
    let object: Object = bcs::from_bytes(&object.serialized_object.unwrap()).unwrap();
    let coin_object_id = object.id();
    let digest = object.digest();
    let version = object.version();
    let previous_transaction = object.as_inner().previous_transaction;
    let coin = object.as_coin_maybe().unwrap();
    let coin_type = object
        .coin_type_maybe()
        .unwrap()
        .to_canonical_string(/* with_prefix */ true);
    Ok(Coin {
        coin_type,
        coin_object_id,
        version,
        digest,
        balance: coin.balance.value(),
        previous_transaction,
    })
}
