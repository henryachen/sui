// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

// This illustrates the "inconsistency" of suix_getCoins, as opposed to graphql's consistency feature.
// We have a coin with balance 3400 at checkpoint 1.
// We update the coin's balance to 1400 at checkpoint 2.
// We query the coin using a cursor specifying checkpoint 1, and expect to see the coin with balance 3400.
// However, the result will be the latest coin balance which is 1400.

//# init --protocol-version 70 --addresses Test=0x0 --accounts A B --simulator --objects-snapshot-min-checkpoint-lag 2

//# programmable --sender A --inputs 12000 @A
//> 0: SplitCoins(Gas, [Input(0)]);
//> 1: TransferObjects([Result(0)], Input(1))

//# programmable --sender A --inputs 3400 @A
//> 0: SplitCoins(Gas, [Input(0)]);
//> 1: TransferObjects([Result(0)], Input(1))

//# view-object 1,0

//# view-object 2,0

//# create-checkpoint

//# run-jsonrpc
{
  "method": "suix_getCoins",
  "params": ["@{A}"]
}

//# programmable --sender A --inputs object(2,0) 2000 @A
//> 0: SplitCoins(Input(0), [Input(1)]);
//> 1: TransferObjects([Result(0)], Input(2))

//# create-checkpoint

//# run-jsonrpc --cursors {"object_id":@{obj_1_0},"cp_sequence_number":1,"coin_balance_bucket":4}
{
  "method": "suix_getCoins",
  "params": ["@{A}", null, "@{cursor_0}"]
}
