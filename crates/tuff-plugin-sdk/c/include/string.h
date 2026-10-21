// SPDX-License-Identifier: MIT
/* The part of string.h the generated bindings use. Implemented in libc.c. */
#ifndef TUFF_SDK_STRING_H
#define TUFF_SDK_STRING_H
#include <stddef.h>
#ifdef __cplusplus
extern "C" {
#endif
void *memcpy(void *dst, const void *src, size_t n);
void *memmove(void *dst, const void *src, size_t n);
void *memset(void *dst, int c, size_t n);
size_t strlen(const char *s);
#ifdef __cplusplus
}
#endif
#endif
