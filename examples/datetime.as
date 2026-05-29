import * as time from "std/time"
import * as date from "std/date"
import * as intl from "std/intl"

// time: durations + monotonic elapsed around a tiny sleep
let start = time.monotonic()
await time.sleep(5)
let elapsed = time.monotonic() - start
print(elapsed >= 5)
print(time.seconds(3))

// date: parse, components, arithmetic, format
let [d, err] = date.parse("2021-06-15T12:30:00Z")
print(d.year)
print(date.format(d, "%Y/%m/%d"))
let nextWeek = date.addDays(d, 7)
print(nextWeek.day)

// intl: locale-aware number formatting + Turkish case
print(intl.formatNumber(1234567, "en-US"))
print(intl.formatNumber(1234567, "de-DE"))
print(intl.caseUpper("istanbul", "tr"))
