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

@protocol SHPrivateScrollWheelEvent <NSObject>
- (instancetype)initWithScrollingDeltaX:(double)dx scrollingDeltaY:(double)dy acceleratedScrollingDeltaX:(double)adx acceleratedScrollingDeltaY:(double)ady scrollPhase:(NSUInteger)phase momentumPhase:(NSUInteger)momentum;
@end

@protocol SHPrivatePointingDevice <NSObject>
- (NSInteger)type;
- (void)sendScrollWheelEvents:(NSArray *)events;
@end

@protocol SHPrivateVirtualMachine <NSObject>
- (NSArray *)_pointingDevices;
@end

BOOL SHSendScrollWheel(VZVirtualMachine *vm, NSInteger dx, NSInteger dy) {
    Class eventClass = NSClassFromString(@"_VZScrollWheelEvent");
    if (eventClass == nil || ![eventClass instancesRespondToSelector:@selector(initWithScrollingDeltaX:scrollingDeltaY:acceleratedScrollingDeltaX:acceleratedScrollingDeltaY:scrollPhase:momentumPhase:)] || ![vm respondsToSelector:@selector(_pointingDevices)]) {
        return NO;
    }
    id<SHPrivatePointingDevice> device = nil;
    for (id<SHPrivatePointingDevice> candidate in [(id<SHPrivateVirtualMachine>)vm _pointingDevices]) {
        if (![candidate respondsToSelector:@selector(type)] || ![candidate respondsToSelector:@selector(sendScrollWheelEvents:)]) {
            continue;
        }
        if (device == nil || [candidate type] == 2) {
            device = candidate;
        }
    }
    if (device == nil) {
        return NO;
    }
    NSMutableArray *events = [NSMutableArray array];
    for (NSInteger i = 0; i < labs(dy); i++) {
        double delta = dy > 0 ? 10 : -10;
        [events addObject:[(id<SHPrivateScrollWheelEvent>)[eventClass alloc] initWithScrollingDeltaX:0 scrollingDeltaY:delta acceleratedScrollingDeltaX:0 acceleratedScrollingDeltaY:delta scrollPhase:0 momentumPhase:0]];
    }
    for (NSInteger i = 0; i < labs(dx); i++) {
        double delta = dx > 0 ? 10 : -10;
        [events addObject:[(id<SHPrivateScrollWheelEvent>)[eventClass alloc] initWithScrollingDeltaX:delta scrollingDeltaY:0 acceleratedScrollingDeltaX:delta acceleratedScrollingDeltaY:0 scrollPhase:0 momentumPhase:0]];
    }
    [device sendScrollWheelEvents:events];
    return YES;
}

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
