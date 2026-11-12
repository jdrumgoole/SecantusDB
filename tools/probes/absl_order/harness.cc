// Records absl 20250512.1 (mongodb-forks 20250512.1-mongo-1) string hashes and
// flat_hash_map iteration orders, for checking SecantusDB's port.
#include <cstdio>
#include <string>
#include <string_view>
#include <vector>
#include "absl/container/flat_hash_map.h"
#include "absl/hash/hash.h"

struct Hasher {  // mongo::StringMapHasher
  size_t operator()(std::string_view s) const {
    return absl::Hash<absl::string_view>{}(absl::string_view(s.data(), s.size()));
  }
};

static std::string hash_input(size_t j, size_t len) {
  std::string s(len, '\0');
  for (size_t t = 0; t < len; ++t) s[t] = static_cast<char>((j * 31 + t * 7 + 1) & 0xff);
  return s;
}

static std::string key(size_t table, size_t i) {
  std::string s = "k" + std::to_string(table) + "." + std::to_string(i);
  s.append((i * 7 + table * 3) % 41, 'x');
  return s;
}

int main() {
  const size_t lens[] = {0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,20,24,31,32,33,40,
                         48,63,64,65,100,128,200,500,1023,1024,1025,2047,2048,2100,3000};
  printf("{\"seed_probe\": %zu,\n \"hashes\": [", Hasher{}(std::string_view()));
  bool first = true;
  for (size_t j = 0; j < 3; ++j)
    for (size_t len : lens) {
      std::string s = hash_input(j, len);
      printf("%s[%zu,%zu,%zu]", first ? "" : ",", j, len, Hasher{}(s));
      first = false;
    }
  printf("],\n \"orders\": [");
  const size_t sizes[] = {1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,20,23,28,29,30,31,
                          40,55,56,57,60,100,111,112,113,150,200,222,223,224,300,447,448,449,600};
  for (size_t t = 0; t < sizeof(sizes) / sizeof(sizes[0]); ++t) {
    std::vector<std::string> keys;
    for (size_t i = 0; i < sizes[t]; ++i) keys.push_back(key(t, i));
    absl::flat_hash_map<std::string_view, const void*, Hasher> m;
    for (size_t i = 0; i < keys.size(); ++i) m.emplace(keys[i], reinterpret_cast<const void*>(i + 1));
    printf("%s\n  [", t ? "," : "");
    bool f = true;
    for (auto& kv : m) {
      printf("%s%zu", f ? "" : ",", reinterpret_cast<size_t>(kv.second) - 1);
      f = false;
    }
    printf("]");
  }
  printf("]}\n");
  return 0;
}
