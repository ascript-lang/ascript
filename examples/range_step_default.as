// Stepped ranges are valid as class field DEFAULTS. A value-position range like
// `0..10 step 2` materializes to an `array<number>`, honoring both the boundary
// (`..` exclusive, `..=` inclusive) and the signed `step`. The default survives a
// build round-trip (`ascript build` -> `.aso` -> `ascript run`) byte-for-byte.

class Series {
    // exclusive upper bound, stride 2 -> [0, 2, 4, 6, 8]
    evens: array<number> = 0..10 step 2
    // inclusive upper bound, stride 3 -> [1, 4, 7, 10]
    triples: array<number> = 1..=10 step 3
    // descending: the direction follows the bounds, stride -2 -> [10, 8, 6, 4, 2]
    countdown: array<number> = 10..0 step -2
}

// `.from({})` applies field defaults without running `init`.
let s = Series.from({})
print(s.evens)
print(s.triples)
print(s.countdown)

// A directly-constructed instance applies the same defaults.
let d = Series()
print(len(d.evens))
