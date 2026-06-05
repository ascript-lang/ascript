// Consumer program exercising bare-specifier package imports:
//   - `lib`        -> the package entry (which itself does a `./helper` import)
//   - `lib/util`   -> a subpath module of the same package
//   - `@scope/x`   -> a scoped package's entry
import { greet } from "lib"
import { shout } from "lib/util"
import { tag } from "@scope/x"

print(greet("world"))
print(shout("loud"))
print(tag())
