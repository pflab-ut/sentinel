import numpy as np
import cv2
from PIL import Image
import binascii


def load_image(path):
    image = cv2.imread(path)
    return image


def canny(img, out_file):
    img_gray = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
    img_blur = cv2.GaussianBlur(img_gray, (3,3), 0)
    canny = cv2.Canny(img_blur, 30, 150)
    cv2.imwrite(out_file, canny)


def dump_file_in_hex(file):
    with open(file, 'rb') as f:
        data = binascii.hexlify(f.read())
        print(data)


if __name__ == '__main__':
    img = load_image("/home/timmy.webp")
    out_file = 'cv2result.jpg'
    canny(img, out_file)
    dump_file_in_hex(out_file)
