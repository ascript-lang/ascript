// Entry module for the self-contained-bundles feature: a multi-file program whose
// whole reachable module graph (`./bundle_util`) is embedded into a ModuleArchive by
// `compile_archive` so the program runs with no source tree on disk.

import { greet, shout } from "./bundle_util"

print(greet("world"))
print(shout("bundled"))
