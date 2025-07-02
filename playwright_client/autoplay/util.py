class Point(object):
    def __init__(self, x: float, y: float, delay: float = 0.0):
        self.x = x
        self.y = y
        self.delay = delay

    def __repr__(self):
        return f"Point(x={self.x}, y={self.y}, delay={self.delay})"
