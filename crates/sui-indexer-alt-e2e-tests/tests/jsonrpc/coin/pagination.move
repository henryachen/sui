// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

// This test is to verify that deleted coins are not included in the result of suix_getCoins.
// We create two coins, of balances 12 and 34, call the rpc method to see bot of them in the results.
// Then we merge the coins and call the rpc method again to see that only the merged coin with
// balance 46 is in the results.

//# init --protocol-version 70 --addresses Test=0x0 --accounts A B --simulator --objects-snapshot-min-checkpoint-lag 2

//# programmable --sender A --inputs 120000 @A
//> 0: SplitCoins(Gas, [Input(0)]);
//> 1: TransferObjects([Result(0)], Input(1))

//# programmable --sender A --inputs 34000 @A
//> 0: SplitCoins(Gas, [Input(0)]);
//> 1: TransferObjects([Result(0)], Input(1))

//# programmable --sender A --inputs 5600 @A
//> 0: SplitCoins(Gas, [Input(0)]);
//> 1: TransferObjects([Result(0)], Input(1))

//# programmable --sender A --inputs 780 @A
//> 0: SplitCoins(Gas, [Input(0)]);
//> 1: TransferObjects([Result(0)], Input(1))

//# programmable --sender A --inputs 90 @A
//> 0: SplitCoins(Gas, [Input(0)]);
//> 1: TransferObjects([Result(0)], Input(1))

//# programmable --sender A --inputs 10 @A
//> 0: SplitCoins(Gas, [Input(0)]);
//> 1: TransferObjects([Result(0)], Input(1))

//# programmable --sender A --inputs 20 @A
//> 0: SplitCoins(Gas, [Input(0)]);
//> 1: TransferObjects([Result(0)], Input(1))


//# programmable --sender A --inputs 30 @A
//> 0: SplitCoins(Gas, [Input(0)]);
//> 1: TransferObjects([Result(0)], Input(1))

//# view-object 4,0

//# view-object 5,0

//# view-object 6,0

//# view-object 7,0

//# create-checkpoint

//# run-jsonrpc
{
  "method": "suix_getCoins",
  "params": ["@{A}", null, null, 3]
}

//# run-jsonrpc --cursors {"o":"@{obj_2_0}","c":1,"b":4}
{
  "method": "suix_getCoins",
  "params": ["@{A}", null, "@{cursor_0}"]
}

//# run-jsonrpc --cursors {"o":"@{obj_7_0}","c":1,"b":1}
{
  "method": "suix_getCoins",
  "params": ["@{A}", null, "@{cursor_0}"]
}

//# programmable --sender A --inputs 500 @A
//> 0: SplitCoins(Gas, [Input(0)]);
//> 1: TransferObjects([Result(0)], Input(1))

//# create-checkpoint

//# run-jsonrpc --cursors {"o":"@{obj_1_0}","c":2,"b":4}
{
  "method": "suix_getCoins",
  "params": ["@{A}", null, "@{cursor_0}"]
}

//# run-jsonrpc --cursors {"o":"@{obj_1_0}","c":1,"b":4}
{
  "method": "suix_getCoins",
  "params": ["@{A}", null, "@{cursor_0}"]
}

