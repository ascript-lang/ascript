// data_pipeline.as
// ---------------------------------------------------------------------------
// A small ETL pipeline, end to end:
//   1. parse an embedded CSV into header-keyed records (csv.parse)
//   2. coerce the string CSV fields into real numbers (convert.parseNumber)
//   3. transform with array.map / array.filter / array.sort / array.reduce
//   4. tidy up text fields with regex.replace and scan with regex.findAll
//   5. emit the result as pretty JSON and as YAML
//
// Every fallible call returns an [value, err] pair, which we always check.
// ---------------------------------------------------------------------------

import * as csv from "std/csv"
import * as json from "std/json"
import * as yaml from "std/yaml"
import * as array from "std/array"
import * as convert from "std/convert"
import * as regex from "std/regex"

// A self-contained CSV "extract". Note the messy region values we will clean up.
const RAW_CSV = `name,region,units,revenue
Widget A,  us-west ,120,4380.50
Widget B,us-east,0,0
Gadget C, eu-central,57,2210.00
Doohickey D,us-west,channels,999
Sprocket E,ap-south,88,5123.75`

fn main() {
  // --- 1. parse ---------------------------------------------------------
  let [rows, parseErr] = csv.parse(RAW_CSV, { header: true })
  if (parseErr != nil) {
    print(`CSV parse failed: ${parseErr.message}`)
    return
  }
  print(`Parsed ${len(rows)} record(s) from CSV`)

  // --- 2 & 3. coerce + transform into clean records ---------------------
  // CSV fields are always strings; parse the numeric columns and normalize
  // the region (regex.replace strips stray spaces). We skip any record whose
  // numeric fields don't parse (e.g. "channels" in the units column).
  let cleaned = []
  for (r of rows) {
    let [units, uErr] = convert.parseNumber(r.units)
    let [revenue, rErr] = convert.parseNumber(r.revenue)
    if (uErr != nil || rErr != nil) {
      print(`  skipping malformed row: ${r.name} (units='${r.units}')`)
      continue
    }
    // regex.replace(pattern, subject, replacement): collapse surrounding
    // whitespace around the region token.
    let region = regex.replace("^\\s+|\\s+$", r.region, "")
    array.push(cleaned, {
      name: r.name,
      region: region,
      units: units,
      revenue: revenue,
    })
  }

  // Keep only records that actually sold something.
  let sold = array.filter(cleaned, (x) => x.units > 0)

  // Derive a unit price per record (map).
  let priced = array.map(sold, (x) => {
    return {
      name: x.name,
      region: x.region,
      units: x.units,
      revenue: x.revenue,
      unitPrice: x.revenue / x.units,
    }
  })

  // Sort by revenue descending (comparator returns <0 to order a before b).
  let ranked = array.sort(priced, (a, b) => b.revenue - a.revenue)

  // Totals via reduce.
  let totalRevenue = array.reduce(ranked, (acc, x) => acc + x.revenue, 0)
  let totalUnits = array.reduce(ranked, (acc, x) => acc + x.units, 0)

  // --- 4. a regex scan over the names -----------------------------------
  // Find every capitalized word token in the joined names.
  let allNames = array.reduce(ranked, (acc, x) => acc + " " + x.name, "")
  let caps = regex.findAll("[A-Z][a-z]+", allNames)
  print(`Capitalized tokens found in names: ${len(caps)} -> ${caps}`)

  // Assemble the final report object.
  let report = {
    generated: "data_pipeline",
    recordCount: len(ranked),
    totalUnits: totalUnits,
    totalRevenue: totalRevenue,
    topSeller: array.get(ranked, 0).name,
    records: ranked,
  }

  // --- 5. emit as JSON and YAML -----------------------------------------
  let [jsonText, jErr] = json.stringify(report, true)
  if (jErr != nil) {
    print(`JSON encode failed: ${jErr.message}`)
    return
  }
  print("\n=== JSON ===")
  print(jsonText)

  let [yamlText, yErr] = yaml.stringify(report)
  if (yErr != nil) {
    print(`YAML encode failed: ${yErr.message}`)
    return
  }
  print("=== YAML ===")
  print(yamlText)
}

main()
