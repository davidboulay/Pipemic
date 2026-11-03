#include <cstdint>
#include <initializer_list>
#include <iostream>
#include <list>
#include <memory>
#include <utility>
#include <vector>
#include <typeinfo>
#include <cstdlib>
#include <unistd.h>
namespace LWT {
enum class PrintDetailLevel : int {};
struct LewittDeviceID { char pad[512]; };
struct LewittDevice;
struct LewittDeviceConf {
  alignas(16) char storage[4096];
  LewittDeviceConf(std::initializer_list<std::pair<unsigned short, unsigned short>>);
};
struct LewittDeviceManager {
  alignas(16) char storage[8192];
  LewittDeviceManager(LewittDeviceConf const&);
  void refresh(std::list<std::shared_ptr<LewittDevice>>&, std::vector<LewittDeviceID>&, std::vector<LewittDeviceID>&);
};
struct SessionAPI { void* impl; unsigned getVersion() const; };
struct SessionAPIPtr {
  SessionAPI* ptr; void* ctrl;
  SessionAPIPtr(const SessionAPIPtr&);   // never called (guaranteed elision); keeps it non-trivial so the ABI returns via x8
};
struct BaseWaveDevice { SessionAPIPtr getSessionAPI() const; int tick(bool); };
struct Wave3Device { int open(bool); };
std::ostream& operator<<(std::ostream&, std::pair<PrintDetailLevel, SessionAPI const&> const&);
}
extern "C" void* __dynamic_cast(const void*, const std::type_info*, const std::type_info*, std::ptrdiff_t);
extern const std::type_info ti_LewittDevice asm("__ZTIN3LWT12LewittDeviceE");
extern const std::type_info ti_BaseWaveDevice asm("__ZTIN3LWT14BaseWaveDeviceE");

int main(int argc, char** argv) {
  int level = argc > 1 ? atoi(argv[1]) : 2;
  auto* conf = new LWT::LewittDeviceConf({{0x0fd9, 0x0070}});
  auto* mgr = new LWT::LewittDeviceManager(*conf);
  auto* devs = new std::list<std::shared_ptr<LWT::LewittDevice>>();
  auto* added = new std::vector<LWT::LewittDeviceID>();
  auto* removed = new std::vector<LWT::LewittDeviceID>();
  mgr->refresh(*devs, *added, *removed);
  std::cerr << "devices: " << devs->size() << "\n";
  for (auto& d : *devs) {
    auto* b = static_cast<LWT::BaseWaveDevice*>(__dynamic_cast(d.get(), &ti_LewittDevice, &ti_BaseWaveDevice, -1));
    { void** vp=*(void***)d.get(); auto* ti=(const std::type_info*)vp[-1]; std::cerr << "dynamic type: " << ti->name() << "\n"; }
    if (!b) { std::cerr << "not a BaseWaveDevice\n"; continue; }
    int o = reinterpret_cast<LWT::Wave3Device*>(d.get())->open(false);
    std::cerr << "open -> " << o << "\n";
    for (int i = 0; i < 5; i++) std::cerr << "tick -> " << b->tick(false) << "\n";
    LWT::SessionAPIPtr sp = b->getSessionAPI();
    if (!sp.ptr || !sp.ptr->impl) { std::cerr << "no session api\n"; continue; }
    std::cerr << "session api version " << sp.ptr->getVersion() << "\n";
    std::cout << std::pair<LWT::PrintDetailLevel, LWT::SessionAPI const&>(LWT::PrintDetailLevel(level), *sp.ptr) << std::endl;
  }
  _exit(0);   // skip destructors of opaque objects
}
