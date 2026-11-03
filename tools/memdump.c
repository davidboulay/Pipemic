#include <libusb.h>
#include <stdio.h>
int main(void){
  libusb_init(NULL);
  libusb_device_handle *h=libusb_open_device_with_vid_pid(NULL,0x0fd9,0x0070);
  if(!h){puts("open failed");return 1;}
  unsigned char b[512];
  for(int id=0; id<32; id++){
    int r=libusb_control_transfer(h,0xA1,0x85,id,0x3300,b,sizeof b,500);
    if(r<=0) continue;
    printf("msg %2d len %3d:", id, r);
    for(int i=0;i<r;i++) printf("%s%02x", i%16?" ":"\n   ", b[i]);
    printf("\n");
  }
  libusb_close(h); libusb_exit(NULL); return 0;
}
