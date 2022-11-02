#include <stdio.h>

int main(int argc, char **argv) {
  int i = 0;
  if (argc < 2) {
    printf("usage: %s <target strings>\n", argv[0]);
    return 1;
  }
  for (int i = 1; i < argc; i++) {
    printf("%s", argv[i]);
    if (i != argc - 1)
      printf(" ");
  }
  return 0;
}
