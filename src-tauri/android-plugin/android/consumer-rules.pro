# Tauri finds plugin classes and commands by reflection.
-keep class dev.vista.android.** { *; }
-keep class com.yausername.** { *; }
# youtubedl-android unpacks Python with Commons Compress, which looks up its
# zip extra-field classes by reflection.
-keep class org.apache.commons.compress.** { *; }
-dontwarn org.apache.commons.compress.**
# WorkManager (new video checks, NewVideos.kt) opens its Room database by
# creating the generated WorkDatabase_Impl by reflection; R8 dropped its
# constructor and the app crashed at start. (Cadence hit this too.)
-keep class * extends androidx.room.RoomDatabase { <init>(...); }
-keep class androidx.work.impl.** { *; }
-keep class * extends androidx.work.ListenableWorker { <init>(...); }
