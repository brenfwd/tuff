---
title: Hello from the tuff blog
description: A placeholder post that exercises the blog pipeline, from frontmatter to feeds.
date: 2026-10-22
author: Brenden Forward
tags: [meta, site]
draft: true
---

This placeholder checks that the blog pipeline renders posts end to end. It is a draft, so production builds leave it out.

## Code

```rust
fn main() {
    println!("hello, tuff");
}
```

## A diff

```diff
 fn give(stack: ItemStack) {
-    inventory.insert(stack.clone());
+    inventory.insert(stack);
 }
```

### Lists and links

- Read the [docs](https://tuff.bforward.me/docs/introduction/).
- Run `cargo t` before you commit.

## Wrapping up

That is all for now.
