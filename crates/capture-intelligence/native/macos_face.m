#import <Foundation/Foundation.h>
#import <Vision/Vision.h>

#include <stdlib.h>
#include <string.h>

static char *captureos_copy_utf8(NSString *value) {
  if (value == nil) {
    return NULL;
  }
  const char *utf8 = [value UTF8String];
  return utf8 == NULL ? NULL : strdup(utf8);
}

static NSString *captureos_error_description(NSError *error) {
  if (error == nil) {
    return @"Vision did not return an error detail";
  }
  return [NSString stringWithFormat:@"domain=%@ code=%ld description=%@",
                                    error.domain,
                                    (long)error.code,
                                    error.localizedDescription ?: @"unavailable"];
}

static char *captureos_json_response(NSArray<NSDictionary *> *faces, NSString *provider, char **out_error) {
  NSDictionary *response = @{
    @"status": @"ready",
    @"provider": provider,
    @"faces": faces,
    @"error": [NSNull null]
  };
  NSError *serialization_error = nil;
  NSData *json = [NSJSONSerialization dataWithJSONObject:response options:0 error:&serialization_error];
  if (json == nil) {
    if (out_error != NULL) {
      *out_error = captureos_copy_utf8(captureos_error_description(serialization_error));
    }
    return NULL;
  }
  NSString *json_string = [[NSString alloc] initWithData:json encoding:NSUTF8StringEncoding];
  return captureos_copy_utf8(json_string);
}

char *captureos_macos_detect_face_rectangles(const char *utf8_path, char **out_error) {
  if (out_error != NULL) {
    *out_error = NULL;
  }
  @autoreleasepool {
    if (utf8_path == NULL || utf8_path[0] == '\0') {
      if (out_error != NULL) {
        *out_error = captureos_copy_utf8(@"CaptureOS received an empty analysis preview path");
      }
      return NULL;
    }
    NSString *path = [[NSString alloc] initWithUTF8String:utf8_path];
    if (path == nil) {
      if (out_error != NULL) {
        *out_error = captureos_copy_utf8(@"Analysis preview path is not valid UTF-8 for Vision");
      }
      return NULL;
    }
    NSURL *url = [NSURL fileURLWithPath:path];
    if (![url isFileURL] || ![[NSFileManager defaultManager] isReadableFileAtPath:path]) {
      if (out_error != NULL) {
        *out_error = captureos_copy_utf8(@"CaptureOS analysis preview is not readable by Vision");
      }
      return NULL;
    }

    VNDetectFaceRectanglesRequest *request = [[VNDetectFaceRectanglesRequest alloc] init];
    // Let the host Vision framework select a supported local device. Explicit CPU-only mode
    // can fail to allocate a BGRA pixel buffer on otherwise usable macOS configurations.
    VNImageRequestHandler *handler = [[VNImageRequestHandler alloc] initWithURL:url options:@{}];
    NSError *error = nil;
    if (![handler performRequests:@[request] error:&error]) {
      if (out_error != NULL) {
        *out_error = captureos_copy_utf8(captureos_error_description(error));
      }
      return NULL;
    }

    NSMutableArray<NSDictionary *> *faces = [NSMutableArray array];
    for (VNFaceObservation *observation in request.results) {
      CGRect box = observation.boundingBox;
      [faces addObject:@{
        @"x": @(box.origin.x),
        @"y": @(1.0 - box.origin.y - box.size.height),
        @"width": @(box.size.width),
        @"height": @(box.size.height),
        @"detectionConfidence": @(observation.confidence),
        @"pose": [NSNull null],
        @"eyeState": @"not_analyzable",
        @"eyeConfidence": [NSNull null]
      }];
    }
    return captureos_json_response(faces, @"apple-vision-face-rectangles", out_error);
  }
}

void captureos_macos_free_string(char *value) {
  free(value);
}
