// functional_pipelines.as
// ---------------------------------------------------------------------------
// Demonstrates map / filter / reduce / sort / groupBy / partition pipelines
// over real data, plus a stream.range pipeline.
//
// Every output is deterministic; no external I/O.
// ---------------------------------------------------------------------------
import * as array from "std/array"
import * as map from "std/map"
import * as stream from "std/stream"

// ---------------------------------------------------------------------------
// 1. Source data: a catalogue of products
// ---------------------------------------------------------------------------
// prices in whole cents to keep arithmetic deterministic across all modes
const PRODUCTS = [{name: "Widget A", category: "hardware", price: 1250, units: 120}, {name: "Widget B", category: "hardware", price: 800, units: 30}, {name: "Gadget C", category: "software", price: 4999, units: 57}, {name: "Doohickey D", category: "hardware", price: 525, units: 0}, {name: "Sprocket E", category: "software", price: 9900, units: 88}, {name: "Gizmo F", category: "hardware", price: 2200, units: 15}, {name: "Module G", category: "software", price: 1999, units: 200}, {name: "Bolt H", category: "hardware", price: 175, units: 500}]

// ---------------------------------------------------------------------------
// 2. map — enrich each record with a computed revenue field
// ---------------------------------------------------------------------------
let enriched = array.map(PRODUCTS, (p) => {
  return {name: p.name, category: p.category, price: p.price, units: p.units, revenue: p.price * p.units}
})

print("=== Enriched (first 3) ===")
for (p of array.slice(enriched, 0, 3)) {
  print(`  ${p.name}: revenue=${p.revenue}`)
}

// ---------------------------------------------------------------------------
// 3. filter — keep only products that actually sold
// ---------------------------------------------------------------------------
let sold = array.filter(enriched, (p) => p.units > 0)
print("")
print(`=== Sold products: ${len(sold)} of ${len(enriched)} ===`)
for (p of sold) {
  print(`  ${p.name} (${p.units} units)`)
}

// ---------------------------------------------------------------------------
// 4. sort — by revenue descending
// ---------------------------------------------------------------------------
let ranked = array.sort(sold, (a, b) => b.revenue - a.revenue)
print("\n=== Top sellers by revenue ===")
for (p of ranked) {
  print(`  ${p.name}: ${p.revenue}`)
}

// ---------------------------------------------------------------------------
// 5. reduce — compute aggregate totals
// ---------------------------------------------------------------------------
let totalRevenue = array.reduce(ranked, (acc, p) => acc + p.revenue, 0)
let totalUnits = array.reduce(ranked, (acc, p) => acc + p.units, 0)
print("\n=== Totals ===")
print(`  revenue: ${totalRevenue}`)
print(`  units:   ${totalUnits}`)

// ---------------------------------------------------------------------------
// 6. groupBy — group sold products by category
// ---------------------------------------------------------------------------
let byCategory = array.groupBy(sold, (p) => p.category)
print("\n=== By category ===")
let hwProducts = map.get(byCategory, "hardware")
let swProducts = map.get(byCategory, "software")
print(`  hardware: ${len(hwProducts)} products`)
print(`  software: ${len(swProducts)} products`)

// ---------------------------------------------------------------------------
// 7. partition — split into high-volume (>=50 units) and low-volume
// ---------------------------------------------------------------------------
let parts = array.partition(sold, (p) => p.units >= 50)
let highVol = parts[0]
let lowVol = parts[1]
print("\n=== Volume partition ===")
print(`  high-volume (>=50): ${len(highVol)}`)
print(`  low-volume  (<50):  ${len(lowVol)}`)

// ---------------------------------------------------------------------------
// 8. stream pipeline — stream.range + map + filter + reduce
//    (exercises the trampoline on the stream side)
// ---------------------------------------------------------------------------
let sumOfEvenSquares = await stream.reduce(stream.filter(stream.map(stream.range(0, 20), (n) => n * n), (sq) => sq % 2 == 0), (acc, sq) => acc + sq, 0)
print("\n=== Stream: sum of even squares 0..20 ===")
print(`  result: ${sumOfEvenSquares}`)

// ---------------------------------------------------------------------------
// 9. Composed pipeline — one expression over the product catalogue
// ---------------------------------------------------------------------------
let hwRevenue = array.reduce(array.filter(enriched, (p) => p.category == "hardware" && p.units > 0), (acc, p) => acc + p.revenue, 0)
print("\n=== Hardware revenue (sold only) ===")
print(`  ${hwRevenue}`)
