#include <libusb.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>
/* Polls config (msg 0) read-only every 100 ms and prints each change. */
int main(int argc,char**argv){
  int secs = argc>1 ? atoi(argv[1]) : 240;
  libusb_init(NULL);
  libusb_device_handle *h=libusb_open_device_with_vid_pid(NULL,0x0fd9,0x0070);
  if(!h){puts("open failed");return 1;}
  unsigned char prev[16]={0}, cur[16]; int have=0;
  time_t t0=time(NULL);
  while(time(NULL)-t0 < secs){
    int r=libusb_control_transfer(h,0xA1,0x85,0,0x3300,cur,16,500);
    if(r==16 && (!have || memcmp(prev,cur,16))){
      printf("t=%3lds ", (long)(time(NULL)-t0));
      for(int i=0;i<16;i++) printf(have&&prev[i]!=cur[i]?"[%02x]":" %02x ",cur[i]);
      printf("\n"); fflush(stdout);
      memcpy(prev,cur,16); have=1;
    }
    usleep(100000);
  }
  return 0;
}
