#include "tuff.hpp"

using namespace tuff;

struct Gems {
    static void declare_content(Content &out) {
        ItemDecl ruby("example:ruby", "minecraft:emerald");
        ruby.model(str("example:ruby")).components(str("{\"minecraft:max_stack_size\":16}"));
        out.item(move(ruby));
        out.entity(str("example:imp"), str("minecraft:zombie"));
        AssetDecl pack("gems", "example");
        pack.file(str("models/item/ruby.json"), str("{}"));
        out.asset(move(pack));
        out.recipe(str("example:ruby_recipe"), str("{\"type\":\"crafting_shapeless\"}"));
        out.advancement(str("example:got_ruby"), str("{}"));
    }
};

TUFF_CAP_CONTENT("example", "Adds the ruby");
TUFF_PLUGIN(Gems, "gems", "0.1.0", "content probe");
