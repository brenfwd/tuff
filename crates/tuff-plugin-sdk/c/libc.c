// SPDX-License-Identifier: MIT
/* The few libc functions the generated bindings and clang need, for a
   freestanding wasm32 build without wasi-libc: a power-of-two size-class
   allocator over memory grown on demand, and the string primitives. */
#include <stddef.h>
#include <stdint.h>

#include <stdlib.h>
#include <string.h>

#define HEADER 16
#define CLASSES 32

extern unsigned char __heap_base;

typedef struct block {
    size_t class;
    struct block *next;
} block_t;

static block_t *free_lists[CLASSES];
static uintptr_t heap_top;

static size_t class_of(size_t size) {
    size_t class = 5;
    while (class < CLASSES && ((size_t)1 << class) < size + HEADER) {
        class++;
    }
    return class;
}

static void *bump(size_t bytes) {
    if (heap_top == 0) {
        heap_top = ((uintptr_t)&__heap_base + 15) & ~(uintptr_t)15;
    }
    uintptr_t start = heap_top;
    uintptr_t end = start + bytes;
    if (end < start) {
        return NULL;
    }
    uintptr_t have = __builtin_wasm_memory_size(0) * 65536;
    if (end > have) {
        size_t pages = (end - have + 65535) / 65536;
        if (__builtin_wasm_memory_grow(0, pages) == (size_t)-1) {
            return NULL;
        }
    }
    heap_top = end;
    return (void *)start;
}

void *malloc(size_t size) {
    size_t class = class_of(size);
    if (class >= CLASSES) {
        return NULL;
    }
    block_t *block = free_lists[class];
    if (block) {
        free_lists[class] = block->next;
    } else {
        block = bump((size_t)1 << class);
        if (!block) {
            return NULL;
        }
        block->class = class;
    }
    return (unsigned char *)block + HEADER;
}

void *calloc(size_t count, size_t size) {
    if (size && count > (size_t)-1 / size) {
        return NULL;
    }
    void *ptr = malloc(count * size);
    if (ptr) {
        memset(ptr, 0, count * size);
    }
    return ptr;
}

void free(void *ptr) {
    if (!ptr) {
        return;
    }
    block_t *block = (block_t *)((unsigned char *)ptr - HEADER);
    block->next = free_lists[block->class];
    free_lists[block->class] = block;
}

void *realloc(void *ptr, size_t size) {
    if (!ptr) {
        return malloc(size);
    }
    block_t *block = (block_t *)((unsigned char *)ptr - HEADER);
    size_t capacity = ((size_t)1 << block->class) - HEADER;
    if (size <= capacity) {
        return ptr;
    }
    void *grown = malloc(size);
    if (grown) {
        memcpy(grown, ptr, capacity);
        free(ptr);
    }
    return grown;
}

_Noreturn void abort(void) {
    __builtin_trap();
}

void *memcpy(void *dst, const void *src, size_t n) {
    unsigned char *d = dst;
    const unsigned char *s = src;
    while (n--) {
        *d++ = *s++;
    }
    return dst;
}

void *memmove(void *dst, const void *src, size_t n) {
    unsigned char *d = dst;
    const unsigned char *s = src;
    if (d < s) {
        while (n--) {
            *d++ = *s++;
        }
    } else {
        while (n--) {
            d[n] = s[n];
        }
    }
    return dst;
}

void *memset(void *dst, int c, size_t n) {
    unsigned char *d = dst;
    while (n--) {
        *d++ = (unsigned char)c;
    }
    return dst;
}

size_t strlen(const char *s) {
    size_t n = 0;
    while (s[n]) {
        n++;
    }
    return n;
}

/* The bindings reference the component-type object so a componentizing
   link keeps it; the host runs the core module as is, so it is skipped
   (bindgen.sh --no-object-file) and this stands in for it. */
void __component_type_object_force_link_plugin(void) {}

/* Static destructors never run: a plugin instance is dropped, not exited. */
void *__dso_handle;

int __cxa_atexit(void (*fn)(void *), void *arg, void *dso) {
    (void)fn;
    (void)arg;
    (void)dso;
    return 0;
}
