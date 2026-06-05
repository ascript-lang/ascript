{
  "targets": [
    {
      "target_name": "tree_sitter_ascript_binding",
      "dependencies": ["<!(node -p \"require('node-addon-api').gyp\")"],
      "include_dirs": ["src"],
      "sources": [
        "bindings/node/binding.cc",
        "src/parser.c"
      ],
      "conditions": [
        ["OS!='win'", { "cflags_c": ["-std=c11"] }]
      ]
    }
  ]
}
