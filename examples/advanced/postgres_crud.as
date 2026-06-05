// postgres_crud.as — a fully error-handled std/postgres tour.
//
// Connect, create a temp table, insert with bind params, query, and close — all
// over the async client. There is NO bundled Postgres, so this example is a no-op
// (prints a skip note and exits 0) unless ASCRIPT_TEST_POSTGRES_URL is set, e.g.:
//
//   docker run -e POSTGRES_PASSWORD=pw -p 5432:5432 -d postgres
//   ASCRIPT_TEST_POSTGRES_URL=postgres://postgres:pw@localhost/postgres \
//     ascript run examples/advanced/postgres_crud.as
import * as postgres from "std/postgres"
import * as env from "std/env"

async fn main() {
  let url = env.get("ASCRIPT_TEST_POSTGRES_URL")
  if (url == nil) {
    print("postgres_crud: ASCRIPT_TEST_POSTGRES_URL not set — skipping live demo (ok)")
    return
  }

  let [conn, cerr] = await postgres.connect(url)
  if (cerr != nil) {
    print(`connect failed: ${cerr.message}`)
    return
  }

  // A session-local temp table keeps the demo isolated and self-cleaning.
  let [_c, e1] = await conn.exec("CREATE TEMP TABLE sp5_demo (id int, name text)")
  if (e1 != nil) { print(`create failed: ${e1.message}`); conn.close(); return }

  let [n, e2] = await conn.exec("INSERT INTO sp5_demo VALUES ($1, $2)", [1, "Ada"])
  if (e2 != nil) { print(`insert failed: ${e2.message}`); conn.close(); return }
  print(`inserted ${n} row(s)`)

  let [rows, e3] = await conn.query("SELECT id, name FROM sp5_demo ORDER BY id")
  if (e3 != nil) { print(`query failed: ${e3.message}`); conn.close(); return }
  for (r of rows) {
    print(`row: id=${r.id} name=${r.name}`)
  }

  let [one, e4] = await conn.queryOne("SELECT name FROM sp5_demo WHERE id = $1", [1])
  if (e4 == nil && one != nil) { print(`queryOne: ${one.name}`) }

  conn.close()
  print("postgres_crud: done")
}

await main()
