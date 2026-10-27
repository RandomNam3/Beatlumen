#include <stdio.h>
#include <stdlib.h>
#include <string.h>
typedef struct OpusMSDecoder OpusMSDecoder;
OpusMSDecoder *opus_multistream_decoder_create(int Fs, int channels, int streams, int coupled, const unsigned char *mapping, int *error);
int opus_multistream_decode_float(OpusMSDecoder *st, const unsigned char *data, int len, float *pcm, int frame_size, int decode_fec);
int opus_multistream_decoder_ctl(OpusMSDecoder *st, int request, ...);
int main(int argc, char **argv) {
  if (argc < 9) return 2;
  int fs = atoi(argv[1]), ch = atoi(argv[2]), streams = atoi(argv[3]), coupled = atoi(argv[4]), gain = atoi(argv[5]);
  unsigned char mapping[256];
  for (int i = 0; i < ch; i++) { unsigned v; sscanf(argv[6] + 2*i, "%2x", &v); mapping[i] = v; }
  FILE *in = fopen(argv[7], "rb"), *out = fopen(argv[8], "wb");
  if (!in || !out) return 3;
  int err = 0;
  OpusMSDecoder *d = opus_multistream_decoder_create(fs, ch, streams, coupled, mapping, &err);
  if (!d) return 4;
  if (gain) opus_multistream_decoder_ctl(d, 4034, gain);
  static unsigned char buf[100000];
  float *pcm = malloc(sizeof(float) * 5760 * 2 * ch);
  int last = fs / 50;
  for (;;) {
    unsigned char l4[4];
    if (fread(l4, 1, 4, in) != 4) break;
    unsigned len = l4[0] | l4[1] << 8 | l4[2] << 16 | (unsigned)l4[3] << 24;
    if (len > sizeof(buf)) return 5;
    if (fread(buf, 1, len, in) != len) return 6;
    int n;
    if (len == 0) n = opus_multistream_decode_float(d, NULL, 0, pcm, last, 0);
    else n = opus_multistream_decode_float(d, buf, len, pcm, 5760 * fs / 48000, 0);
    if (n < 0) { fprintf(stderr, "err %d\n", n); continue; }
    if (len) last = n;
    fwrite(pcm, sizeof(float), n * ch, out);
  }
  fclose(out);
  return 0;
}
