// Imports a package that is NOT in the resolved set → "unknown package" error,
// byte-identical on both engines.
import { x } from "missing"

print(x)
