// smtp_send.as — send a message with std/email through an in-script SMTP sink.
//
// A real SMTP server is not needed (and would make the example non-deterministic):
// we stand up a minimal in-process SMTP SINK over std/net/tcp that speaks just
// enough of the protocol (220 → EHLO/250 → MAIL/250 → RCPT/250 → DATA/354 →
// body/250 → QUIT/221), captures the envelope + DATA payload, and lets the
// program exit. email.send connects to it with tls:"none".
//
// Single-threaded model: email.send runs as a spawned task; the main task drives
// the sink. connect() lands in the OS listen backlog, so accept() never deadlocks.
import * as email from "std/email"
import * as tcp from "std/net/tcp"
import * as string from "std/string"
import * as task from "std/task"

// Speak the SMTP dialog with one connected client; return the captured envelope.
async fn runSink(conn) {
  let captured = {from: nil, rcpts: [], subject: nil, dataLines: 0}

  // Greeting.
  await conn.write("220 sink.local ESMTP ready\r\n")
  let inData = false
  while (true) {
    let line = await conn.readLine()
    if (line == nil) {
      break
    }
    if (inData) {
      // The DATA payload is terminated by a lone "." line.
      if (string.trim(line) == ".") {
        inData = false
        await conn.write("250 OK: queued\r\n")
        continue
      }
      captured.dataLines = captured.dataLines + 1
      if (string.startsWith(line, "Subject:")) {
        captured.subject = string.trim(string.slice(line, len("Subject:"), len(line)))
      }
      continue
    }
    let upper = string.upper(line)
    if (string.startsWith(upper, "EHLO") || string.startsWith(upper, "HELO")) {
      // Single-line 250 with NO STARTTLS (we asked for tls:"none").
      await conn.write("250 sink.local\r\n")
    } else if (string.startsWith(upper, "MAIL FROM:")) {
      captured.from = extractAddr(line)
      await conn.write("250 OK\r\n")
    } else if (string.startsWith(upper, "RCPT TO:")) {
      captured.rcpts = [...captured.rcpts, extractAddr(line)]
      await conn.write("250 OK\r\n")
    } else if (string.startsWith(upper, "DATA")) {
      inData = true
      await conn.write("354 start mail input; end with <CRLF>.<CRLF>\r\n")
    } else if (string.startsWith(upper, "QUIT")) {
      await conn.write("221 bye\r\n")
      break
    } else {
      await conn.write("250 OK\r\n")
    }
  }
  return captured
}

// Pull the address out of "MAIL FROM:<a@b>" / "RCPT TO:<a@b>".
fn extractAddr(line) {
  let lt = string.find(line, "<")
  let gt = string.find(line, ">")
  if (lt < 0 || gt < 0) {
    return string.trim(line)
  }
  return string.slice(line, lt + 1, gt)
}

async fn main() {
  let [server, e1] = tcp.listen("127.0.0.1", 0)
  if (e1 != nil) {
    print(`listen error: ${e1.message}`)
    return
  }
  let port = server.port

  // Build the message (pure builder — deterministic, no Date header stamped).
  let [msg, merr] = email.message({from: "alice@example.com", to: "bob@example.com", cc: "carol@example.com", subject: "Deployment complete", text: "The release is live.\n.dotted line is stuffed by the sender\nbye"})
  if (merr != nil) {
    print(`build error: ${merr.message}`)
    return
  }

  // Spawn the SEND; the main task accepts + drives the sink.
  let sendTask = task.spawn((async () => {
    return await email.send(msg, {host: "127.0.0.1", port: port, tls: "none"})
  })())
  let [conn, e3] = await server.accept()
  if (e3 != nil) {
    print(`accept error: ${e3.message}`)
    return
  }
  let captured = await runSink(conn)
  conn.close()
  server.close()

  // Collect the send result.
  let [result, serr] = await sendTask
  if (serr != nil) {
    print(`send error: ${serr.message}`)
    return
  }
  print(`accepted: ${string.join(result.accepted, ", ")}`)
  print(`rejected count: ${len(result.rejected)}`)
  print(`sink saw MAIL FROM: ${captured.from}`)
  print(`sink saw RCPT TO: ${string.join(captured.rcpts, ", ")}`)
  print(`sink saw Subject: ${captured.subject}`)
  print(`sink received DATA body lines > 0: ${captured.dataLines > 0}`)
}

await main()
print("smtp_send ok")
