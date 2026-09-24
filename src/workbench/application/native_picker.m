// A separate process keeps AppKit's main thread and localization independent
// of the gateway. A physical application bundle also exposes localization to
// the separate system process which draws NSOpenPanel.
#import <AppKit/AppKit.h>
#include <stdio.h>
#include <string.h>

static int writeJSON(id value) {
    NSError *error = nil;
    NSData *data = [NSJSONSerialization dataWithJSONObject:value
                                                 options:NSJSONWritingFragmentsAllowed
                                                   error:&error];
    if (data == nil) {
        fputs("picker_serialization_failed\n", stderr);
        return 1;
    }
    if (fwrite(data.bytes, 1, data.length, stdout) != data.length || fflush(stdout) != 0) {
        return 1;
    }
    return 0;
}

@interface FolderPickerDelegate : NSObject <NSApplicationDelegate>
@property BOOL probe;
@property BOOL finishedLaunching;
@property int status;
@property(strong) id result;
@property(strong) NSOpenPanel *panel;
@end

@implementation FolderPickerDelegate
- (void)finishWithResult:(id)result status:(int)status {
    self.result = result;
    self.status = status;
    [NSApp stop:nil];
    // stop: only takes effect after the current event is dispatched. Completion
    // may arrive from XPC or the main queue, so wake the event loop ourselves.
    [NSApp postEvent:[NSEvent otherEventWithType:NSEventTypeApplicationDefined
                                      location:NSZeroPoint
                                 modifierFlags:0
                                     timestamp:0
                                  windowNumber:0
                                       context:nil
                                       subtype:0
                                         data1:0
                                         data2:0]
             atStart:NO];
}
- (void)complete:(NSModalResponse)response {
    @try {
        if (response == NSModalResponseCancel) {
            [self finishWithResult:NSNull.null status:0];
        } else if (response == NSModalResponseOK) {
            NSURL *url = self.panel.URL;
            NSString *path = url.path;
            if (!url.isFileURL || !path.isAbsolutePath) {
                fputs("picker_invalid_result\n", stderr);
                [self finishWithResult:nil status:1];
            } else {
                [self finishWithResult:path status:0];
            }
        } else {
            fputs("picker_unavailable\n", stderr);
            [self finishWithResult:nil status:1];
        }
    } @catch (NSException *exception) {
        (void)exception;
        fputs("picker_unavailable\n", stderr);
        [self finishWithResult:nil status:1];
    }
}
- (void)showPicker {
    @try {
        if (self.probe) {
            [self finishWithResult:@{
                @"running": @(NSApp.isRunning),
                @"finishedLaunching": @(self.finishedLaunching),
                @"mainThread": @(NSThread.isMainThread),
            } status:0];
            return;
        }
        self.panel = NSOpenPanel.openPanel;
        self.panel.canChooseDirectories = YES;
        self.panel.canChooseFiles = NO;
        self.panel.allowsMultipleSelection = NO;
        self.panel.canCreateDirectories = NO;
        [self.panel beginWithCompletionHandler:^(NSModalResponse response) {
            [self complete:response];
        }];
        // Activate after ordering the panel, with the normal application event
        // loop running, rather than before window creation during startup.
        [NSApp activateIgnoringOtherApps:YES];
    } @catch (NSException *exception) {
        (void)exception;
        fputs("picker_unavailable\n", stderr);
        [self finishWithResult:nil status:1];
    }
}
- (void)applicationDidFinishLaunching:(NSNotification *)notification {
    (void)notification;
    self.finishedLaunching = YES;
    // The launch notification precedes event processing. Let AppKit finish
    // startup before connecting the remote panel and requesting input focus.
    dispatch_async(dispatch_get_main_queue(), ^{ [self showPicker]; });
}
@end

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        @try {
            // Inspect localization without constructing an application or panel.
            // Optional AppleLanguages is a process-only Foundation argument for
            // regression probes; it never writes the user's preferences.
            BOOL probeArguments = argc == 2 ||
                (argc == 4 && strcmp(argv[2], "-AppleLanguages") == 0);
            if (probeArguments && strcmp(argv[1], "--localization-probe") == 0) {
                return writeJSON(@{
                    @"main": NSBundle.mainBundle.preferredLocalizations,
                    @"appkit": [NSBundle bundleForClass:NSOpenPanel.class].preferredLocalizations,
                    @"bundlePath": NSBundle.mainBundle.bundleURL.path,
                });
            }
            BOOL eventLoopProbe = argc == 2 && strcmp(argv[1], "--event-loop-probe") == 0;
            if (argc != 1 && !eventLoopProbe) {
                return 64;
            }
            NSApplication *app = NSApplication.sharedApplication;
            [app setActivationPolicy:NSApplicationActivationPolicyAccessory];
            FolderPickerDelegate *delegate = [FolderPickerDelegate new];
            delegate.probe = eventLoopProbe;
            delegate.status = 1;
            app.delegate = delegate;
            [app run];
            return delegate.status == 0 ? writeJSON(delegate.result) : delegate.status;
        } @catch (NSException *exception) {
            // Native diagnostics must not disclose the selected path or payload.
            (void)exception;
            fputs("picker_unavailable\n", stderr);
            return 1;
        }
    }
}
