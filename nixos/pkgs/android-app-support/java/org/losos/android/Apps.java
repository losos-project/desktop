package org.losos.android;

import android.content.Context;
import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.drawable.Drawable;
import android.os.Looper;
import java.io.File;
import java.io.FileOutputStream;
import java.io.OutputStream;
import java.lang.reflect.Constructor;
import java.lang.reflect.Field;
import java.lang.reflect.Method;
import java.util.List;

/**
 * The Pixel's launchable apps, as its own launcher sees them: each one's
 * package, name and icon. Run by app_process as adb's shell user, which may
 * read every app's package (docs/android-apps.md, "The apps").
 *
 * <pre>
 *   Apps list         package TAB name TAB system, one launchable app a line
 *   Apps icons DIR N  DIR/package.png for each, N pixels square
 * </pre>
 *
 * The icons are drawn the way the launcher draws them: an adaptive icon in
 * the phone's icon shape, a vector one rasterised, so what the desktop
 * shows is what the phone shows, whatever form the app ships it in.
 */
public final class Apps {
    public static void main(String[] args) throws Exception {
        Looper.prepareMainLooper();
        PackageManager pm = systemContext().getPackageManager();
        Intent launcher = new Intent("android.intent.action.MAIN");
        launcher.addCategory("android.intent.category.LAUNCHER");
        List<ResolveInfo> apps = pm.queryIntentActivities(launcher, 0);

        if (args.length == 1 && args[0].equals("list")) {
            StringBuilder out = new StringBuilder();
            for (ResolveInfo app : apps) {
                ActivityInfo activity = app.activityInfo;
                // ApplicationInfo.FLAG_SYSTEM: it came with the phone.
                boolean system = (activity.applicationInfo.flags & 1) != 0;
                out.append(activity.packageName).append('\t')
                    .append(oneLine(app.loadLabel(pm))).append('\t')
                    .append(system ? "system" : "user").append('\n');
            }
            System.out.print(out);
        } else if (args.length == 3 && args[0].equals("icons")) {
            File dir = new File(args[1]);
            int size = Integer.parseInt(args[2]);
            dir.mkdirs();
            for (ResolveInfo app : apps) {
                // One app with an icon that will not draw keeps its old one
                // (or the desktop's fallback), not everyone else's.
                try {
                    Drawable icon = app.loadIcon(pm);
                    Bitmap bitmap = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888);
                    icon.setBounds(0, 0, size, size);
                    icon.draw(new Canvas(bitmap));
                    try (OutputStream file = new FileOutputStream(
                            new File(dir, app.activityInfo.packageName + ".png"))) {
                        bitmap.compress(Bitmap.CompressFormat.PNG, 100, file);
                    }
                } catch (Exception e) {
                    System.err.println(app.activityInfo.packageName + ": " + e);
                }
            }
        } else {
            System.err.println("usage: Apps list | Apps icons DIR SIZE");
            System.exit(2);
        }
    }

    /** A name for a desktop entry, which ends at a line's end. */
    private static String oneLine(CharSequence label) {
        return label.toString().replaceAll("[\\t\\r\\n]+", " ").trim();
    }

    /**
     * The system context an app_process program can have, the way scrcpy's
     * server gets one: an ActivityThread of its own made current, then
     * asked. ActivityThread is not in the SDK, so it is reached by name.
     */
    private static Context systemContext() throws Exception {
        Class<?> threadClass = Class.forName("android.app.ActivityThread");
        Constructor<?> constructor = threadClass.getDeclaredConstructor();
        constructor.setAccessible(true);
        Object thread = constructor.newInstance();
        Field current = threadClass.getDeclaredField("sCurrentActivityThread");
        current.setAccessible(true);
        current.set(null, thread);
        Method getSystemContext = threadClass.getDeclaredMethod("getSystemContext");
        return (Context) getSystemContext.invoke(thread);
    }
}
