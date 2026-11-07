#import <AVFoundation/AVFoundation.h>
#import <CoreVideo/CoreVideo.h>
#import <Foundation/Foundation.h>

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

@interface FesRecordingEncoder : NSObject
@property(nonatomic, strong) AVAssetWriter *writer;
@property(nonatomic, strong) AVAssetWriterInput *input;
@property(nonatomic, strong) AVAssetWriterInputPixelBufferAdaptor *adaptor;
@property(nonatomic) uint32_t width;
@property(nonatomic) uint32_t height;
@property(nonatomic) uint64_t nextFrame;
@property(nonatomic) BOOL finished;
@end

@implementation FesRecordingEncoder
@end

static void set_error(char *buffer, size_t size, NSString *message) {
    if (buffer == NULL || size == 0) {
        return;
    }
    const char *text = message.UTF8String;
    snprintf(buffer, size, "%s", text != NULL ? text : "Apple video encoder failed");
}

static NSString *writer_error(AVAssetWriter *writer, NSString *fallback) {
    NSError *error = writer.error;
    return error != nil ? error.localizedDescription : fallback;
}

bool festerm_apple_exclude_recording_directory_from_backup(const char *path,
                                                            char *error, size_t error_size) {
    @autoreleasepool {
        NSString *filename = [NSString stringWithUTF8String:path];
        if (filename == nil) {
            set_error(error, error_size, @"Invalid private recording directory");
            return false;
        }
        NSURL *url = [NSURL fileURLWithPath:filename isDirectory:YES];
        NSError *native_error = nil;
        if (![url setResourceValue:@YES forKey:NSURLIsExcludedFromBackupKey error:&native_error]) {
            set_error(error, error_size, @"Could not exclude private recording storage from backup");
            return false;
        }
        id value = nil;
        if (![url getResourceValue:&value forKey:NSURLIsExcludedFromBackupKey error:&native_error]
            || ![value respondsToSelector:@selector(boolValue)] || ![value boolValue]) {
            set_error(error, error_size, @"Could not verify recording storage backup exclusion");
            return false;
        }
        return true;
    }
}

void *festerm_apple_video_create(const char *path, uint32_t width, uint32_t height,
                                 char *error, size_t error_size) {
    @autoreleasepool {
        NSString *filename = [NSString stringWithUTF8String:path];
        if (filename == nil) {
            set_error(error, error_size, @"Invalid private video path");
            return NULL;
        }
        NSURL *url = [NSURL fileURLWithPath:filename];
        NSError *native_error = nil;
        AVAssetWriter *writer = [[AVAssetWriter alloc] initWithURL:url
                                                         fileType:AVFileTypeMPEG4
                                                            error:&native_error];
        if (writer == nil) {
            set_error(error, error_size, native_error.localizedDescription);
            return NULL;
        }

        uint64_t bits = (uint64_t)width * height * 6;
        bits = bits < 1000000 ? 1000000 : (bits > 50000000 ? 50000000 : bits);
        NSDictionary *compression = @{
            AVVideoAverageBitRateKey: @(bits),
            AVVideoExpectedSourceFrameRateKey: @30,
            AVVideoMaxKeyFrameIntervalKey: @30,
            AVVideoAllowFrameReorderingKey: @NO,
            AVVideoProfileLevelKey: AVVideoProfileLevelH264HighAutoLevel,
        };
        NSDictionary *settings = @{
            AVVideoCodecKey: AVVideoCodecTypeH264,
            AVVideoWidthKey: @(width),
            AVVideoHeightKey: @(height),
            AVVideoCompressionPropertiesKey: compression,
        };
        if (![writer canApplyOutputSettings:settings forMediaType:AVMediaTypeVideo]) {
            set_error(error, error_size, @"H.264 encoding is unavailable for this video size");
            return NULL;
        }
        AVAssetWriterInput *input = [AVAssetWriterInput assetWriterInputWithMediaType:AVMediaTypeVideo
                                                                     outputSettings:settings];
        input.expectsMediaDataInRealTime = NO;
        if (![writer canAddInput:input]) {
            set_error(error, error_size, @"The MP4 writer cannot accept a video track");
            return NULL;
        }
        [writer addInput:input];
        NSDictionary *attributes = @{
            (NSString *)kCVPixelBufferPixelFormatTypeKey: @(kCVPixelFormatType_32BGRA),
            (NSString *)kCVPixelBufferWidthKey: @(width),
            (NSString *)kCVPixelBufferHeightKey: @(height),
        };
        AVAssetWriterInputPixelBufferAdaptor *adaptor =
            [[AVAssetWriterInputPixelBufferAdaptor alloc] initWithAssetWriterInput:input
                                                     sourcePixelBufferAttributes:attributes];
        writer.directoryForTemporaryFiles = [url URLByDeletingLastPathComponent];
        if (![writer startWriting]) {
            set_error(error, error_size, writer_error(writer, @"Could not start the MP4 writer"));
            return NULL;
        }
        [writer startSessionAtSourceTime:kCMTimeZero];

        FesRecordingEncoder *encoder = [[FesRecordingEncoder alloc] init];
        encoder.writer = writer;
        encoder.input = input;
        encoder.adaptor = adaptor;
        encoder.width = width;
        encoder.height = height;
        return (__bridge_retained void *)encoder;
    }
}

bool festerm_apple_video_append(void *handle, const uint8_t *rgba, size_t length,
                                uint64_t index, char *error, size_t error_size) {
    @autoreleasepool {
        FesRecordingEncoder *encoder = (__bridge FesRecordingEncoder *)handle;
        if (encoder == nil || rgba == NULL || encoder.finished ||
            length != (size_t)encoder.width * encoder.height * 4 ||
            index != encoder.nextFrame || index > INT64_MAX) {
            set_error(error, error_size, @"Invalid video frame");
            return false;
        }
        NSTimeInterval deadline = NSProcessInfo.processInfo.systemUptime + 30.0;
        while (!encoder.input.readyForMoreMediaData) {
            if (encoder.writer.status != AVAssetWriterStatusWriting) {
                set_error(error, error_size, writer_error(encoder.writer, @"The MP4 writer stopped"));
                return false;
            }
            if (NSProcessInfo.processInfo.systemUptime >= deadline) {
                set_error(error, error_size, @"The MP4 writer timed out accepting a frame");
                return false;
            }
            [NSThread sleepForTimeInterval:0.01];
        }
        CVPixelBufferPoolRef pool = encoder.adaptor.pixelBufferPool;
        CVPixelBufferRef buffer = NULL;
        if (pool == NULL || CVPixelBufferPoolCreatePixelBuffer(kCFAllocatorDefault, pool, &buffer) != kCVReturnSuccess ||
            buffer == NULL) {
            set_error(error, error_size, @"Could not allocate a video frame");
            return false;
        }
        CVReturn locked = CVPixelBufferLockBaseAddress(buffer, 0);
        if (locked != kCVReturnSuccess) {
            CVPixelBufferRelease(buffer);
            set_error(error, error_size, @"Could not lock a video frame");
            return false;
        }
        uint8_t *base = CVPixelBufferGetBaseAddress(buffer);
        size_t stride = CVPixelBufferGetBytesPerRow(buffer);
        if (base == NULL || stride < (size_t)encoder.width * 4) {
            if (CVPixelBufferUnlockBaseAddress(buffer, 0) != kCVReturnSuccess) {
                CVPixelBufferRelease(buffer);
                set_error(error, error_size, @"Could not unlock a video frame");
                return false;
            }
            CVPixelBufferRelease(buffer);
            set_error(error, error_size, @"Invalid native video frame layout");
            return false;
        }
        for (size_t y = 0; y < encoder.height; y++) {
            uint8_t *destination = base + y * stride;
            const uint8_t *source = rgba + y * (size_t)encoder.width * 4;
            for (size_t x = 0; x < encoder.width; x++) {
                destination[4 * x] = source[4 * x + 2];
                destination[4 * x + 1] = source[4 * x + 1];
                destination[4 * x + 2] = source[4 * x];
                destination[4 * x + 3] = 255;
            }
        }
        if (CVPixelBufferUnlockBaseAddress(buffer, 0) != kCVReturnSuccess) {
            CVPixelBufferRelease(buffer);
            set_error(error, error_size, @"Could not unlock a video frame");
            return false;
        }
        BOOL appended = [encoder.adaptor appendPixelBuffer:buffer
                                      withPresentationTime:CMTimeMake((int64_t)index, 30)];
        CVPixelBufferRelease(buffer);
        if (!appended) {
            set_error(error, error_size, writer_error(encoder.writer, @"Could not append a video frame"));
            return false;
        }
        encoder.nextFrame++;
        return true;
    }
}

bool festerm_apple_video_finish(void *handle, char *error, size_t error_size) {
    @autoreleasepool {
        FesRecordingEncoder *encoder = (__bridge FesRecordingEncoder *)handle;
        if (encoder == nil || encoder.finished || encoder.nextFrame == 0) {
            set_error(error, error_size, @"Cannot finish an empty or completed video");
            return false;
        }
        [encoder.input markAsFinished];
        dispatch_semaphore_t done = dispatch_semaphore_create(0);
        [encoder.writer finishWritingWithCompletionHandler:^{
            dispatch_semaphore_signal(done);
        }];
        if (dispatch_semaphore_wait(done, dispatch_time(DISPATCH_TIME_NOW, 120 * NSEC_PER_SEC)) != 0) {
            set_error(error, error_size, @"The MP4 writer timed out while finalizing");
            return false;
        }
        if (encoder.writer.status != AVAssetWriterStatusCompleted) {
            set_error(error, error_size, writer_error(encoder.writer, @"Could not finalize the MP4"));
            return false;
        }
        encoder.finished = YES;
        return true;
    }
}

void festerm_apple_video_destroy(void *handle) {
    @autoreleasepool {
        if (handle == NULL) {
            return;
        }
        FesRecordingEncoder *encoder = (__bridge_transfer FesRecordingEncoder *)handle;
        if (!encoder.finished && encoder.writer.status == AVAssetWriterStatusWriting) {
            [encoder.writer cancelWriting];
        }
    }
}
