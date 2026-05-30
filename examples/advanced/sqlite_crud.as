// sqlite_crud.as
// ---------------------------------------------------------------------------
// CRUD against an in-memory SQLite database, exercising the whole API:
//   sqlite.open(":memory:")           -> [conn, err]
//   conn.exec(sql, params?)           -> [changes, err]   (DDL/DML)
//   conn.prepare(sql)                 -> [stmt, err]
//   stmt.run(params)                  -> [changes, err]
//   conn.query(sql, params?)          -> [rows, err]      (SELECT)
//   conn.begin() / conn.commit()      -> [nil, err]       (transactions)
//   conn.close()                      -> [nil, err]
//
// Positional params bind to "?" placeholders ([a, b]); a params OBJECT binds
// named ":id" placeholders ({ id: 1 }). Rows come back as an array of objects
// keyed by column name. Every call returns an [value, err] pair.
// ---------------------------------------------------------------------------

import * as sqlite from "std/sqlite"

fn main() {
  let [conn, openErr] = sqlite.open(":memory:")
  if (openErr != nil) {
    print(`open failed: ${openErr.message}`)
    return
  }
  print("Opened in-memory database")

  // --- schema -----------------------------------------------------------
  let [ddlOut, ddlErr] = conn.exec(`
    CREATE TABLE users (
      id    INTEGER PRIMARY KEY,
      name  TEXT NOT NULL,
      email TEXT NOT NULL,
      score INTEGER NOT NULL DEFAULT 0
    )
  `)
  if (ddlErr != nil) {
    print(`CREATE TABLE failed: ${ddlErr.message}`)
    return
  }

  // --- CREATE: a couple of direct inserts with positional params --------
  let seed = [
    [1, "Ada Lovelace", "ada@example.com", 95],
    [2, "Alan Turing", "alan@example.com", 98],
  ]
  for (u of seed) {
    let [changes, insErr] = conn.exec(
      "INSERT INTO users (id, name, email, score) VALUES (?, ?, ?, ?)",
      u
    )
    if (insErr != nil) {
      print(`INSERT failed: ${insErr.message}`)
      return
    }
  }
  print(`Inserted ${len(seed)} users directly`)

  // --- CREATE via a prepared statement, reused for several rows ---------
  let [stmt, prepErr] = conn.prepare(
    "INSERT INTO users (id, name, email, score) VALUES (?, ?, ?, ?)"
  )
  if (prepErr != nil) {
    print(`prepare failed: ${prepErr.message}`)
    return
  }
  let more = [
    [3, "Grace Hopper", "grace@example.com", 99],
    [4, "Edsger Dijkstra", "edsger@example.com", 91],
  ]
  let prepCount = 0
  for (u of more) {
    let [changes, runErr] = stmt.run(u)
    if (runErr != nil) {
      print(`stmt.run failed: ${runErr.message}`)
      return
    }
    prepCount = prepCount + changes
  }
  print(`Inserted ${prepCount} users via prepared statement`)

  // --- UPDATE inside a transaction --------------------------------------
  let [beginOut, beginErr] = conn.begin()
  if (beginErr != nil) {
    print(`begin failed: ${beginErr.message}`)
    return
  }
  let [bumped, updErr] = conn.exec("UPDATE users SET score = score + 1 WHERE score < ?", [99])
  if (updErr != nil) {
    print(`UPDATE failed: ${updErr.message}`)
    return
  }
  let [commitOut, commitErr] = conn.commit()
  if (commitErr != nil) {
    print(`commit failed: ${commitErr.message}`)
    return
  }
  print(`Transaction committed: bumped ${bumped} row(s) by +1`)

  // --- READ: a single row by NAMED param ({ id } binds :id) -------------
  let [oneRows, q1Err] = conn.query("SELECT name, score FROM users WHERE id = :id", { id: 1 })
  if (q1Err != nil) {
    print(`named query failed: ${q1Err.message}`)
    return
  }
  if (len(oneRows) > 0) {
    let row = oneRows[0]
    print(`User #1 by named param: ${row.name} (score ${row.score})`)
  }

  // --- READ: full leaderboard, iterate the row objects ------------------
  let [rows, qErr] = conn.query("SELECT id, name, email, score FROM users ORDER BY score DESC")
  if (qErr != nil) {
    print(`SELECT failed: ${qErr.message}`)
    return
  }

  print(`\n=== Leaderboard (${len(rows)} users) ===`)
  let rank = 1
  for (r of rows) {
    print(`  ${rank}. ${r.name}  <${r.email}>  score=${r.score}`)
    rank = rank + 1
  }

  // --- always release the connection (close() returns nil, not a pair) --
  conn.close()
  print("\nConnection closed cleanly")
}

main()
