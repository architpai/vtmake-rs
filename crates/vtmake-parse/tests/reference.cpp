#include <string>
#include <cstring>
#include "jsonpull/jsonpull.h"
#include "geometry.hpp"
#include "serial.hpp"
#include "read_json.hpp"
extern "C" size_t bt_dtoa_reference(double v, char *out) {
    auto s = milo::dtoa_milo(v); memcpy(out,s.data(),s.size()); return s.size();
}
extern "C" size_t bt_attrs_reference(const char *input, char *out, size_t cap) {
    auto jp=json_begin_string(input); auto root=json_read_tree(jp);
    if(!root) return 0;
    auto props=json_hash_get(root.get(),"properties");
    std::string result;
    if(props && props->type==JSON_HASH) for(auto &entry:props->entries()) {
        auto sv=stringify_value(entry.value.get(),"reference",1,root.get());
        auto type=entry.value->type;
        char tag= type==JSON_NUMBER?'n':type==JSON_TRUE||type==JSON_FALSE?'b':type==JSON_NULL?'0':'s';
        result+=entry.key->string();result.push_back('\0');result.push_back(tag);result+=sv.s;result.push_back('\0');
    }
    if(result.size()<=cap) memcpy(out,result.data(),result.size()); return result.size();
}
extern "C" size_t bt_json_reference(const char *input) {
    auto jp=json_begin_string(input); auto root=json_read_tree(jp);
    if(!root) return 0;
    auto props=json_hash_get(root.get(),"properties");
    return props && props->type==JSON_HASH ? props->entries().size() : 0;
}
