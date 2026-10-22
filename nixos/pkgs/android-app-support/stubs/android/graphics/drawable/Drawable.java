package android.graphics.drawable;

public abstract class Drawable {
    public void setBounds(int left, int top, int right, int bottom) { throw new RuntimeException("Stub!"); }
    public abstract void draw(android.graphics.Canvas canvas);
}
