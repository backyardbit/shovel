#import <Foundation/Foundation.h>
#import <Virtualization/Virtualization.h>

NS_ASSUME_NONNULL_BEGIN

@interface SHVNCServer : NSObject
- (nullable instancetype)initWithVirtualMachine:(VZVirtualMachine *)vm
                                       password:(NSString *)password
                                          queue:(dispatch_queue_t)queue;
- (void)start;
- (void)stop;
- (NSInteger)port;
@end

BOOL SHSendScrollWheel(VZVirtualMachine *vm, NSInteger dx, NSInteger dy);

NS_ASSUME_NONNULL_END
