const root = require("path").join(__dirname, "..", "..");

const binding = require("node-gyp-build")(root);

try {
  module.exports = binding.language;
} catch (_) {}

try {
  module.exports.nodeTypeInfo = require("../../src/node-types.json");
} catch (_) {}
