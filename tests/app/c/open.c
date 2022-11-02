#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <unistd.h>

int main() {
  const char *filename = "tmptmp";
  int fd = open(filename, O_CREAT | O_WRONLY);
  printf("opened new file %s: fd is %d\n", filename, fd);
  const char *content = "This is a temporary file";
  int res = write(fd, content, strlen(content));
  if (res < 0) {
      printf("Failed to write to %s\n", filename);
      return 1;
  }
  printf("wrote %d bytes.\n", res);
  close(fd);
  fd = open(filename, O_RDONLY);
  char *buf = calloc(res + 1, sizeof(char));
  res = read(fd, buf, res);
  if (res < 0) {
      printf("Failed to read %s\n", filename);
      return 1;
  }
  printf("File content is %d bytes: %s\n", res, buf);
  return 0;
}
