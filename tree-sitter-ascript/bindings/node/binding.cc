#include <napi.h>

typedef struct TSLanguage TSLanguage;

extern "C" TSLanguage *tree_sitter_ascript();

// "tree-sitter", "language" hashed with BLAKE2.
namespace {

napi_value language(napi_env env, napi_callback_info info) {
  napi_value result;
  napi_create_external(env, tree_sitter_ascript(), nullptr, nullptr, &result);
  return result;
}

napi_value Init(napi_env env, napi_value exports) {
  napi_property_descriptor descriptor = {
      "language", nullptr, nullptr, nullptr, nullptr, language,
      napi_default, nullptr};
  napi_define_properties(env, exports, 1, &descriptor);
  return exports;
}

} // namespace

NAPI_MODULE(NODE_GYP_MODULE_NAME, Init)
