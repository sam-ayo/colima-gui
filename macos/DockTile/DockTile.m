// The Dock tile plug-in of Colima GUI. The Dock loads it when the app is in
// the Dock and does not run. It shows the icon that the app saved for the
// selected theme, and it shows the icon dim when no Colima machine runs.
//
// The app saves the icons to
// ~/Library/Application Support/<bundle ID>/DockIcon (see
// src/ui/app_icon.rs). Without these files, the Dock shows the bundle icon.
//
// scripts/package.sh builds this file into
// Contents/PlugIns/DockTile.docktileplugin.

#import <AppKit/AppKit.h>
#include <signal.h>

// The plug-in checks the Colima state at this interval, in seconds. The app
// checks it at the same interval (see src/state/machine_store.rs).
static const NSTimeInterval kRefresh = 10;

@interface ColimaGUIDockTile : NSObject <NSDockTilePlugIn>
@property(strong) NSDockTile *tile;
@property(strong) NSTimer *timer;
// The path and the date of the icon on the tile. The plug-in draws the tile
// again only when one of them changes.
@property(copy) NSString *shown;
@end

@implementation ColimaGUIDockTile

// The bundle ID of the app that contains this plug-in. The app uses it as
// the name of its user defaults and of its Application Support folder.
- (NSString *)appID {
    NSURL *url = [[NSBundle bundleForClass:[self class]] bundleURL];
    // DockTile.docktileplugin is in Colima GUI.app/Contents/PlugIns.
    for (int i = 0; i < 3; i++) {
        url = [url URLByDeletingLastPathComponent];
    }
    return [[NSBundle bundleWithURL:url] bundleIdentifier] ?: @"colima-gui";
}

- (NSString *)stringPreference:(NSString *)key app:(CFStringRef)app {
    CFPropertyListRef value = CFPreferencesCopyAppValue((__bridge CFStringRef)key, app);
    id object = CFBridgingRelease(value);
    return [object isKindOfClass:[NSString class]] ? object : nil;
}

// True if the app has the dark appearance. The app saves the choice of the
// View menu as "Appearance". "System" follows System Settings.
- (BOOL)isDark {
    CFStringRef app = (__bridge CFStringRef)[self appID];
    CFPreferencesAppSynchronize(app);
    NSString *choice = [self stringPreference:@"Appearance" app:app];
    if ([choice isEqualToString:@"Dark"]) {
        return YES;
    }
    if ([choice isEqualToString:@"Light"]) {
        return NO;
    }
    CFPreferencesAppSynchronize(kCFPreferencesAnyApplication);
    NSString *style = [self stringPreference:@"AppleInterfaceStyle" app:kCFPreferencesAnyApplication];
    return [style isEqualToString:@"Dark"];
}

// True if a Colima machine runs. The Lima host agent of a running machine
// writes its process ID to ha.pid, and removes the file when the machine
// stops. The app does the same check (see src/colima.rs).
- (BOOL)isRunning {
    NSString *lima = [NSHomeDirectory() stringByAppendingPathComponent:@".colima/_lima"];
    NSArray<NSString *> *names = [[NSFileManager defaultManager] contentsOfDirectoryAtPath:lima error:nil];
    for (NSString *name in names) {
        NSString *file = [[lima stringByAppendingPathComponent:name] stringByAppendingPathComponent:@"ha.pid"];
        NSString *text = [NSString stringWithContentsOfFile:file encoding:NSUTF8StringEncoding error:nil];
        int pid = [[text stringByTrimmingCharactersInSet:[NSCharacterSet whitespaceAndNewlineCharacterSet]] intValue];
        // Signal 0 sends no signal. It only checks that the process exists.
        if (pid > 0 && kill(pid, 0) == 0) {
            return YES;
        }
    }
    return NO;
}

- (void)update {
    NSDockTile *tile = self.tile;
    if (tile == nil) {
        return;
    }
    NSString *support = [NSHomeDirectory() stringByAppendingPathComponent:@"Library/Application Support"];
    NSString *name = [NSString stringWithFormat:@"%@-%@.png", [self isDark] ? @"dark" : @"light",
                                                [self isRunning] ? @"running" : @"stopped"];
    NSString *path = [[[support stringByAppendingPathComponent:[self appID]]
        stringByAppendingPathComponent:@"DockIcon"] stringByAppendingPathComponent:name];
    NSDate *date = [[[NSFileManager defaultManager] attributesOfItemAtPath:path error:nil] fileModificationDate];
    NSString *shown = [NSString stringWithFormat:@"%@ %f", path, date.timeIntervalSinceReferenceDate];
    if ([shown isEqualToString:self.shown]) {
        return;
    }
    self.shown = shown;
    NSImage *image = date ? [[NSImage alloc] initWithContentsOfFile:path] : nil;
    // Without a view, the Dock shows the bundle icon.
    tile.contentView = image ? [NSImageView imageViewWithImage:image] : nil;
    [tile display];
}

// The Dock gives a tile when it loads the plug-in, and nil when the app
// starts. The app then sets its own icon.
- (void)setDockTile:(NSDockTile *)tile {
    [self.timer invalidate];
    self.timer = nil;
    [[NSDistributedNotificationCenter defaultCenter] removeObserver:self];
    self.tile = tile;
    self.shown = nil;
    if (tile == nil) {
        return;
    }
    [self update];
    self.timer = [NSTimer scheduledTimerWithTimeInterval:kRefresh
                                                  target:self
                                                selector:@selector(update)
                                                userInfo:nil
                                                 repeats:YES];
    // Change the icon at once when System Settings changes the appearance.
    [[NSDistributedNotificationCenter defaultCenter] addObserver:self
                                                        selector:@selector(update)
                                                            name:@"AppleInterfaceThemeChangedNotification"
                                                          object:nil];
}

@end
