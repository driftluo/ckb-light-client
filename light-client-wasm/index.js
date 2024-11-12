import {
  ligth_client,
  get_tip_header,
  SetScriptsCommand,
  get_script_example,
  set_scripts,
  get_cells,
  get_search_key_example,
  get_scripts,
  Order,
  stop,
  test_idb,
  test_serde,
  test_indexed_db,
} from "./pkg";
debugger;
// await test_idb();
// await test_indexed_db();
// console.log(test_serde());
// console.log("start 13");
// ckb_vm_test("13");
// console.log("end 13");

// setTimeout(() => {
//   console.log("start 12");
//   ckb_vm_test("12");
//   console.log("end 12");
// }, 2000);

await ligth_client("dev");

function timeout(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

await timeout(2000);
console.log("tip header:", await get_tip_header());

await timeout(5000);

console.log("get script:", await get_scripts());

console.log("get example:", get_script_example());
await set_scripts([get_script_example()], SetScriptsCommand.All);

await timeout(2000);
console.log("get script delay:", await get_scripts());

async function f() {
  const t = setInterval(async function () {
    console.log(
      "get cells:",
      await get_cells(get_search_key_example(), Order.Asc, 20, null)
    );
  }, 10000);
}
await f();
