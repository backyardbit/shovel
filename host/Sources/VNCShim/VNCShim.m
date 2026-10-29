#import "VNCShim.h"

@protocol SHPrivateVNCAuthentication <NSObject>
- (instancetype)initWithPassword:(NSString *)password;
@end

@protocol SHPrivateVNCServer <NSObject>
- (instancetype)initWithPort:(NSInteger)port queue:(dispatch_queue_t)queue securityConfiguration:(id)configuration;
- (void)setVirtualMachine:(VZVirtualMachine *)virtualMachine;
- (void)start;
- (void)stop;
- (NSInteger)port;
@end

@implementation SHVNCServer {
    id<SHPrivateVNCServer> _server;
}

- (instancetype)initWithVirtualMachine:(VZVirtualMachine *)vm password:(NSString *)password queue:(dispatch_queue_t)queue {
    Class authClass = NSClassFromString(@"_VZVNCAuthenticationSecurityConfiguration");
    Class serverClass = NSClassFromString(@"_VZVNCServer");
    if (authClass == nil || serverClass == nil) {
        return nil;
    }
    self = [super init];
    if (self) {
        id auth = [(id<SHPrivateVNCAuthentication>)[authClass alloc] initWithPassword:password];
        _server = [(id<SHPrivateVNCServer>)[serverClass alloc] initWithPort:0 queue:queue securityConfiguration:auth];
        [_server setVirtualMachine:vm];
    }
    return self;
}

- (void)start {
    [_server start];
}

- (void)stop {
    [_server stop];
}

- (NSInteger)port {
    return [_server port] & 0xFFFF;
}

@end
