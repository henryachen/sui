// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use move_binary_format::errors::PartialVMResult;
use move_core_types::{account_address::AccountAddress, gas_algebra::InternalGas};
use move_vm_runtime::{native_charge_gas_early_exit, native_functions::NativeContext};
use move_vm_types::{
    loaded_data::runtime_types::Type,
    natives::function::NativeResult,
    pop_arg,
    values::{Struct, Value, Vector},
};
use smallvec::smallvec;
use std::{collections::VecDeque, convert::TryFrom};
use sui_types::base_types::{ObjectID, SuiAddress, TransactionDigest};

use crate::{
    object_runtime::{ObjectRuntime, TransactionContext},
    NativesCostTable,
};

#[derive(Clone)]
pub struct TxContextDeriveIdCostParams {
    pub tx_context_derive_id_cost_base: InternalGas,
}
/***************************************************************************************************
 * native fun derive_id
 * Implementation of the Move native function `fun derive_id(tx_hash: vector<u8>, ids_created: u64): address`
 *   gas cost: tx_context_derive_id_cost_base                | we operate on fixed size data structures
 **************************************************************************************************/
pub fn derive_id(
    context: &mut NativeContext,
    ty_args: Vec<Type>,
    mut args: VecDeque<Value>,
) -> PartialVMResult<NativeResult> {
    debug_assert!(ty_args.is_empty());
    debug_assert!(args.len() == 2);

    let tx_context_derive_id_cost_params = context
        .extensions_mut()
        .get::<NativesCostTable>()
        .tx_context_derive_id_cost_params
        .clone();
    native_charge_gas_early_exit!(
        context,
        tx_context_derive_id_cost_params.tx_context_derive_id_cost_base
    );

    let ids_created = pop_arg!(args, u64);
    let tx_hash = pop_arg!(args, Vec<u8>);

    // unwrap safe because all digests in Move are serialized from the Rust `TransactionDigest`
    let digest = TransactionDigest::try_from(tx_hash.as_slice()).unwrap();
    let address = AccountAddress::from(ObjectID::derive_id(digest, ids_created));
    let obj_runtime: &mut ObjectRuntime = context.extensions_mut().get_mut();
    obj_runtime.new_id(address.into())?;

    Ok(NativeResult::ok(
        context.gas_used(),
        smallvec![Value::address(address)],
    ))
}

#[derive(Clone)]
pub struct TxContextSenderCostParams {
    pub tx_context_sender_cost_base: InternalGas,
}
#[allow(dead_code)]
pub fn sender(
    context: &mut NativeContext,
    ty_args: Vec<Type>,
    args: VecDeque<Value>,
) -> PartialVMResult<NativeResult> {
    debug_assert!(ty_args.is_empty());
    debug_assert!(args.is_empty());

    let tx_context_sender_cost_params = context
        .extensions_mut()
        .get::<NativesCostTable>()
        .tx_context_sender_cost_params
        .clone();
    native_charge_gas_early_exit!(
        context,
        tx_context_sender_cost_params.tx_context_sender_cost_base
    );

    let transaction_context: &mut TransactionContext = context.extensions_mut().get_mut();
    let sender = transaction_context.sender();

    Ok(NativeResult::ok(
        context.gas_used(),
        smallvec![Value::address(sender.into())],
    ))
}

#[derive(Clone)]
pub struct TxContextEpochCostParams {
    pub tx_context_epoch_cost_base: InternalGas,
}
#[allow(dead_code)]
pub fn epoch(
    context: &mut NativeContext,
    ty_args: Vec<Type>,
    args: VecDeque<Value>,
) -> PartialVMResult<NativeResult> {
    debug_assert!(ty_args.is_empty());
    debug_assert!(args.is_empty());

    let tx_context_epoch_cost_params = context
        .extensions_mut()
        .get::<NativesCostTable>()
        .tx_context_epoch_cost_params
        .clone();
    native_charge_gas_early_exit!(
        context,
        tx_context_epoch_cost_params.tx_context_epoch_cost_base
    );

    let transaction_context: &mut TransactionContext = context.extensions_mut().get_mut();
    let epoch = transaction_context.epoch();

    Ok(NativeResult::ok(
        context.gas_used(),
        smallvec![Value::u64(epoch)],
    ))
}

#[derive(Clone)]
pub struct TxContextEpochTimestampMsCostParams {
    pub tx_context_epoch_timestamp_ms_cost_base: InternalGas,
}
pub fn epoch_timestamp_ms(
    context: &mut NativeContext,
    ty_args: Vec<Type>,
    args: VecDeque<Value>,
) -> PartialVMResult<NativeResult> {
    debug_assert!(ty_args.is_empty());
    debug_assert!(args.is_empty());

    let tx_context_epoch_timestamp_ms_cost_params = context
        .extensions_mut()
        .get::<NativesCostTable>()
        .tx_context_epoch_timestamp_ms_cost_params
        .clone();
    native_charge_gas_early_exit!(
        context,
        tx_context_epoch_timestamp_ms_cost_params.tx_context_epoch_timestamp_ms_cost_base
    );

    let transaction_context: &mut TransactionContext = context.extensions_mut().get_mut();
    let timestamp = transaction_context.epoch_timestamp_ms();

    Ok(NativeResult::ok(
        context.gas_used(),
        smallvec![Value::u64(timestamp)],
    ))
}

#[derive(Clone)]
pub struct TxContextSponsorCostParams {
    pub tx_context_sponsor_cost_base: InternalGas,
}
pub fn sponsor(
    context: &mut NativeContext,
    ty_args: Vec<Type>,
    args: VecDeque<Value>,
) -> PartialVMResult<NativeResult> {
    debug_assert!(ty_args.is_empty());
    debug_assert!(args.is_empty());

    let tx_context_sponsor_cost_params = context
        .extensions_mut()
        .get::<NativesCostTable>()
        .tx_context_sponsor_cost_params
        .clone();
    native_charge_gas_early_exit!(
        context,
        tx_context_sponsor_cost_params.tx_context_sponsor_cost_base
    );

    let transaction_context: &mut TransactionContext = context.extensions_mut().get_mut();
    let sponsor = to_option(transaction_context.sponsor())?;

    Ok(NativeResult::ok(context.gas_used(), smallvec![sponsor]))
}

fn to_option(value: Option<SuiAddress>) -> PartialVMResult<Value> {
    let vector = Type::Vector(Box::new(Type::Address));
    match value {
        Some(value) => {
            let value = vec![AccountAddress::new(value.to_inner())];
            Ok(Value::struct_(Struct::pack(vec![Vector::pack(
                &vector,
                vec![Value::vector_address(value)],
            )?])))
        }
        None => Ok(Value::struct_(Struct::pack(vec![Vector::empty(&vector)?]))),
    }
}

#[derive(Clone)]
pub struct TxContextIdsCreatedCostParams {
    pub tx_context_ids_created_cost_base: InternalGas,
}

pub fn ids_created(
    context: &mut NativeContext,
    ty_args: Vec<Type>,
    args: VecDeque<Value>,
) -> PartialVMResult<NativeResult> {
    debug_assert!(ty_args.is_empty());
    debug_assert!(args.is_empty());

    let tx_context_ids_created_cost_params = context
        .extensions_mut()
        .get::<NativesCostTable>()
        .tx_context_ids_created_cost_params
        .clone();
    native_charge_gas_early_exit!(
        context,
        tx_context_ids_created_cost_params.tx_context_ids_created_cost_base
    );

    let transaction_context: &mut TransactionContext = context.extensions_mut().get_mut();
    let ids_created = transaction_context.ids_created();

    Ok(NativeResult::ok(
        context.gas_used(),
        smallvec![Value::u64(ids_created)],
    ))
}

#[derive(Clone)]
pub struct TxContextFreshIdCostParams {
    pub tx_context_fresh_id_cost_base: InternalGas,
}
pub fn fresh_id(
    context: &mut NativeContext,
    ty_args: Vec<Type>,
    args: VecDeque<Value>,
) -> PartialVMResult<NativeResult> {
    debug_assert!(ty_args.is_empty());
    debug_assert!(args.is_empty());

    let tx_context_fresh_id_cost_params = context
        .extensions_mut()
        .get::<NativesCostTable>()
        .tx_context_fresh_id_cost_params
        .clone();
    native_charge_gas_early_exit!(
        context,
        tx_context_fresh_id_cost_params.tx_context_fresh_id_cost_base
    );

    let transaction_context: &mut TransactionContext = context.extensions_mut().get_mut();
    let fresh_id = transaction_context.fresh_id();
    let object_runtime: &mut ObjectRuntime = context.extensions_mut().get_mut();
    object_runtime.new_id(fresh_id)?;

    Ok(NativeResult::ok(
        context.gas_used(),
        smallvec![Value::address(fresh_id.into())],
    ))
}

// //
// // Test only function
// //

#[derive(Clone)]
pub struct TxContextIncEpochCostParams {
    pub tx_context_inc_epoch_cost_base: InternalGas,
}
pub fn inc_epoch(
    context: &mut NativeContext,
    ty_args: Vec<Type>,
    args: VecDeque<Value>,
) -> PartialVMResult<NativeResult> {
    debug_assert!(ty_args.is_empty());
    debug_assert!(args.is_empty());

    let tx_context_inc_epoch_cost_params = context
        .extensions_mut()
        .get::<NativesCostTable>()
        .tx_context_inc_epoch_cost_params
        .clone();
    native_charge_gas_early_exit!(
        context,
        tx_context_inc_epoch_cost_params.tx_context_inc_epoch_cost_base
    );

    let transaction_context: &mut TransactionContext = context.extensions_mut().get_mut();
    transaction_context.inc_epoch();

    Ok(NativeResult::ok(context.gas_used(), smallvec![]))
}

#[derive(Clone)]
pub struct TxContextIncEpochTimestampCostParams {
    pub tx_context_inc_epoch_timestamp_cost_base: InternalGas,
}
pub fn inc_epoch_timestamp(
    context: &mut NativeContext,
    ty_args: Vec<Type>,
    mut args: VecDeque<Value>,
) -> PartialVMResult<NativeResult> {
    debug_assert!(ty_args.is_empty());
    debug_assert!(args.len() == 1);

    let tx_context_inc_epoch_cost_params = context
        .extensions_mut()
        .get::<NativesCostTable>()
        .tx_context_inc_epoch_cost_params
        .clone();
    native_charge_gas_early_exit!(
        context,
        tx_context_inc_epoch_cost_params.tx_context_inc_epoch_cost_base
    );

    let delta_ms = pop_arg!(args, u64);
    let transaction_context: &mut TransactionContext = context.extensions_mut().get_mut();
    transaction_context.inc_epoch_timestamp(delta_ms);

    Ok(NativeResult::ok(context.gas_used(), smallvec![]))
}

#[derive(Clone)]
pub struct TxContextReplaceCostParams {
    pub tx_context_replace_cost_base: InternalGas,
}
pub fn replace(
    context: &mut NativeContext,
    ty_args: Vec<Type>,
    mut args: VecDeque<Value>,
) -> PartialVMResult<NativeResult> {
    debug_assert!(ty_args.is_empty());
    debug_assert!(args.len() == 5);

    let tx_context_inc_epoch_cost_params = context
        .extensions_mut()
        .get::<NativesCostTable>()
        .tx_context_inc_epoch_cost_params
        .clone();
    native_charge_gas_early_exit!(
        context,
        tx_context_inc_epoch_cost_params.tx_context_inc_epoch_cost_base
    );

    let sender: AccountAddress = pop_arg!(args, AccountAddress);
    let tx_hash: Vec<u8> = pop_arg!(args, Vec<u8>);
    let epoch: u64 = pop_arg!(args, u64);
    let epoch_timestamp_ms: u64 = pop_arg!(args, u64);
    let ids_created: u64 = pop_arg!(args, u64);
    let transaction_context: &mut TransactionContext = context.extensions_mut().get_mut();
    transaction_context.replace(sender, tx_hash, epoch, epoch_timestamp_ms, ids_created);

    Ok(NativeResult::ok(context.gas_used(), smallvec![]))
}
