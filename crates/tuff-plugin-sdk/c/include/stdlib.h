// SPDX-License-Identifier: MIT
/* The part of stdlib.h the generated bindings and the SDK use, for a freestanding
   build without a libc. Implemented in libc.c. */
#ifndef TUFF_SDK_STDLIB_H
#define TUFF_SDK_STDLIB_H
#include <stddef.h>
#ifdef __cplusplus
extern "C" {
#endif
void *malloc(size_t size);
void *calloc(size_t count, size_t size);
void *realloc(void *ptr, size_t size);
void free(void *ptr);
__attribute__((noreturn)) void abort(void);
#ifdef __cplusplus
}
#endif
#endif
