// Synchronous snapshot of an owned decoded CVPixelBuffer, no capture or UI.
#import <CoreImage/CoreImage.h>
#import <CoreVideo/CoreVideo.h>
#import <dispatch/dispatch.h>

// Status: 0 completed, 1 unsupported buffer, 2 missing color, 3 render failure.
int weavelo_observation_rgba(CVPixelBufferRef buffer, size_t width, size_t height,
                             unsigned char *output, size_t length) {
    if (!buffer || !output || !width || !height || width > 8192 || height > 8192 ||
        width * height * 4 != length || length > 16 * 1024 * 1024 ||
        CVPixelBufferGetWidth(buffer) != width || CVPixelBufferGetHeight(buffer) != height)
        return 1;
    OSType format = CVPixelBufferGetPixelFormatType(buffer);
    if (format != kCVPixelFormatType_420YpCbCr8BiPlanarFullRange &&
        format != kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange) return 1;

    @autoreleasepool {
        @try {
            // Require actual matrix, primaries and transfer attachments. Core
            // Image consumes range from the pixel format. Never invent BT.709
            // for a source whose metadata was omitted by its encoder/decoder.
            NSDictionary *attachments = CFBridgingRelease(
                CVBufferCopyAttachments(buffer, kCVAttachmentMode_ShouldPropagate));
            if (!attachments) return 2;
            for (NSString *key in @[(__bridge NSString *)kCVImageBufferYCbCrMatrixKey,
                                    (__bridge NSString *)kCVImageBufferColorPrimariesKey,
                                    (__bridge NSString *)kCVImageBufferTransferFunctionKey]) {
                if (![attachments[key] isKindOfClass:NSString.class]) return 2;
            }
            NSArray *matrices = @[(__bridge NSString *)kCVImageBufferYCbCrMatrix_ITU_R_709_2,
                                  (__bridge NSString *)kCVImageBufferYCbCrMatrix_ITU_R_601_4,
                                  (__bridge NSString *)kCVImageBufferYCbCrMatrix_SMPTE_240M_1995,
                                  (__bridge NSString *)kCVImageBufferYCbCrMatrix_ITU_R_2020];
            if (![matrices containsObject:attachments[(__bridge NSString *)kCVImageBufferYCbCrMatrixKey]]) return 2;
            CGColorSpaceRef source = CVImageBufferCreateColorSpaceFromAttachments(
                (__bridge CFDictionaryRef)attachments);
            if (!source) return 2;
            CIImage *image = [CIImage imageWithCVPixelBuffer:buffer
                options:@{kCIImageColorSpace: (__bridge id)source,
                          kCIImageApplyOrientationProperty: @NO}];
            CGColorSpaceRelease(source);
            if (!image || !CGRectEqualToRect(image.extent, CGRectMake(0, 0, width, height))) return 1;

            static CIContext *context;
            static dispatch_once_t once;
            dispatch_once(&once, ^{
                context = [CIContext contextWithOptions:@{kCIContextCacheIntermediates: @NO}];
            });
            if (!context) return 3;
            CIRenderDestination *destination = [[CIRenderDestination alloc]
                initWithBitmapData:output width:width height:height
                bytesPerRow:width * 4 format:kCIFormatRGBA8];
            CGColorSpaceRef srgb = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
            if (!destination || !srgb) {
                if (srgb) CGColorSpaceRelease(srgb);
                return 3;
            }
            destination.colorSpace = srgb;
            CGColorSpaceRelease(srgb);
            destination.alphaMode = CIRenderDestinationAlphaNone;
            destination.flipped = YES; // Row zero is the source's top row.
            destination.dithered = NO;
            NSError *error = nil;
            CIRenderTask *task = [context startTaskToRender:image
                toDestination:destination error:&error];
            if (!task) return 3;
            BOOL submittedWithoutError = error == nil;
            error = nil;
            // Do not return the Rust-owned output until all native writes end.
            CIRenderInfo *completed = [task waitUntilCompletedAndReturnError:&error];
            return completed && submittedWithoutError && !error ? 0 : 3;
        } @catch (NSException *exception) {
            // No raw framework exceptions or source metadata in logs.
            return 3;
        }
    }
}
