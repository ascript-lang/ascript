import * as os from "std/os"
import * as net from "std/net"
import * as udp from "std/net/udp"
import * as encoding from "std/encoding"
let pid = os.pid()
assert(pid > 0, `pid() must be positive, got: ${pid}`)
let platform = os.platform()
assert(len(platform) > 0, "platform() must be non-empty")
let arch = os.arch()
assert(len(arch) > 0, "arch() must be non-empty")
let cpus = os.cpuCount()
assert(cpus >= 1, `cpuCount() must be >= 1, got: ${cpus}`)
let host = os.hostname()
assert(len(host) > 0, "hostname() must be non-empty")
let mem = os.memory()
assert(mem.total > 0, `memory().total must be > 0, got: ${mem.total}`)
print(`os: pid=${pid} platform=${platform} arch=${arch} cpus=${cpus} hostname=${host}`)
print(`os: memory total=${mem.total} bytes`)
let [ips, dnsErr] = await net.lookup("localhost")
assert(dnsErr == nil, "net.lookup('localhost') must succeed")
assert(len(ips) >= 1, "net.lookup('localhost') must return at least one IP")
let foundLoopback = false
for (ip of ips) {
  if (ip == "127.0.0.1" || ip == "::1") {
    foundLoopback = true
  }
}
assert(foundLoopback, "net.lookup('localhost') must include 127.0.0.1 or ::1")
print(`net: lookup('localhost') -> ${len(ips)} address(es), first=${ips[0]}`)
let [sockA, errA] = udp.bind("127.0.0.1:0")
assert(errA == nil, "udp.bind sockA failed")
let [sockB, errB] = udp.bind("127.0.0.1:0")
assert(errB == nil, "udp.bind sockB failed")
let addrB = sockB.localAddr()
assert(len(addrB) > 0, "sockB.localAddr() must be non-empty")
let payload = "host_info udp echo"
let [sent, sendErr] = await sockA.send(payload, addrB)
assert(sendErr == nil, "sockA.send failed")
assert(sent > 0, "sockA.send must report > 0 bytes sent")
let [pkt, recvErr] = await sockB.recv()
assert(recvErr == nil, "sockB.recv failed")
let [text, decErr] = encoding.utf8Decode(pkt.data)
assert(decErr == nil, "utf8Decode failed")
assert(text == payload, `UDP echo mismatch: expected '${payload}' got '${text}'`)
assert(len(pkt.from) > 0, "recv pkt.from must be non-empty")
print(`udp: sent ${sent} bytes to ${addrB}, received from ${pkt.from}`)
sockA.close()
sockB.close()
print("host info: all assertions passed")
