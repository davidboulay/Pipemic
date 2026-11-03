#include <libusb.h>
#include <stdio.h>
int main(void){
  libusb_init(NULL);
  libusb_device_handle *h=libusb_open_device_with_vid_pid(NULL,0x0fd9,0x0070);
  if(!h){puts("open failed");return 1;}
  unsigned char b[64]; int r;
  for(int itf=0; itf<5; itf++){
    /* Legacy UAC2: class|interface IN, MEM(3), wValue=10, wIndex=0x33<<8|itf */
    r=libusb_control_transfer(h,0xA1,0x03,10,0x3300|itf,b,2,500);
    printf("legacy uac2 itf%d r=%d %s", itf, r, r>=2?"":"\n"); if(r>=2) printf("ver %d.%d\n",b[0],b[1]);
    r=libusb_control_transfer(h,0xA1,0x85,10,0x3300|itf,b,2,500);
    printf("legacy uac1 itf%d r=%d %s", itf, r, r>=2?"":"\n"); if(r>=2) printf("ver %d.%d\n",b[0],b[1]);
    /* MK2: vendor|interface IN, bRequest 0 */
    r=libusb_control_transfer(h,0xC1,0x00,0,itf,b,2,500);
    printf("mk2 itf%d r=%d %s", itf, r, r>=2?"":"\n"); if(r>=2) printf("ver %d.%d\n",b[0],b[1]);
  }
  libusb_close(h); libusb_exit(NULL); return 0;
}
