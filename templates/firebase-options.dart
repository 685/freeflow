
  static String? _optional(String value) => value.isEmpty ? null : value;


  static FirebaseOptions _firebaseOptions() {
    const webId = String.fromEnvironment('FIREBASE_APP_ID');
    const webKey = String.fromEnvironment('FIREBASE_API_KEY');
    var appId = webId;
    var apiKey = webKey;
    String? iosBundleId;
    String? iosClientId;
    if (!kIsWeb) {
      switch (defaultTargetPlatform) {
        case TargetPlatform.android:
          appId = const String.fromEnvironment('FIREBASE_ANDROID_APP_ID');
          const androidKey = String.fromEnvironment('FIREBASE_ANDROID_API_KEY');
          apiKey = androidKey.isEmpty ? webKey : androidKey;
          break;
        case TargetPlatform.iOS:
          appId = const String.fromEnvironment('FIREBASE_IOS_APP_ID');
          const iosKey = String.fromEnvironment('FIREBASE_IOS_API_KEY');
          apiKey = iosKey.isEmpty ? webKey : iosKey;
          iosBundleId = _optional(const String.fromEnvironment('FIREBASE_IOS_BUNDLE_ID'));
          iosClientId = _optional(const String.fromEnvironment('FIREBASE_IOS_CLIENT_ID'));
          break;
        default:
          throw UnsupportedError('Freeflow Firebase supports web, Android and iOS.');
      }
    }
    if (appId.isEmpty || apiKey.isEmpty) throw StateError('Add Firebase settings for this platform in Freeflow.');
    return FirebaseOptions(
      apiKey: apiKey,
      appId: appId,
      projectId: const String.fromEnvironment('FIREBASE_PROJECT_ID'),
      messagingSenderId: const String.fromEnvironment('FIREBASE_SENDER_ID'),
      authDomain: _optional(const String.fromEnvironment('FIREBASE_AUTH_DOMAIN')),
      storageBucket: _optional(const String.fromEnvironment('FIREBASE_STORAGE_BUCKET')),
      databaseURL: _optional(const String.fromEnvironment('FIREBASE_DATABASE_URL')),
      measurementId: kIsWeb ? _optional(const String.fromEnvironment('FIREBASE_MEASUREMENT_ID')) : null,
      iosBundleId: iosBundleId,
      iosClientId: iosClientId,
    );
  }
