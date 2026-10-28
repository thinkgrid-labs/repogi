#include <stdio.h>
#include <stdint.h>
#include "blake3.h"
int main(void) {
  blake3_hasher h; blake3_hasher_init(&h);
  uint8_t buf[65536], out[32]; size_t n;
  while ((n = fread(buf, 1, sizeof(buf), stdin))) blake3_hasher_update(&h, buf, n);
  if (ferror(stdin)) return 1;
  blake3_hasher_finalize(&h, out, sizeof(out));
  printf("blake3:"); for (size_t i=0;i<sizeof(out);i++) printf("%02x",out[i]);
  puts(""); return ferror(stdout) ? 1 : 0;
}
