# cf) https://github.com/tensorflow/tensorflow/blob/master/tensorflow/lite/examples/python/label_image.py
from PIL import Image

import numpy as np
import tflite_runtime.interpreter as tflite


def load_image(path, width, height):
    return Image.open(path).resize((width, height))


def load_labels(filename):
    with open(filename, 'r') as f:
        return [line.strip() for line in f.readlines()]


if __name__ == '__main__':
    mobilenet_tflite_path = '/home/mobilenet_v2_1.0_224_quant.tflite'
    interpreter = tflite.Interpreter(
        model_path=mobilenet_tflite_path,
        num_threads=4,
    )
    interpreter.allocate_tensors()
    input_details = interpreter.get_input_details()
    output_details = interpreter.get_output_details()

    floating_model = input_details[0]['dtype'] == np.float32

    height = input_details[0]['shape'][1]
    width = input_details[0]['shape'][2]

    img = load_image('/home/grace_hopper.bmp', width, height)
    input_data = np.expand_dims(img, axis=0)

    if floating_model:
        input_data = (np.float32(input_data) - 127.5) / 127.5

    interpreter.set_tensor(input_details[0]['index'], input_data)
    interpreter.invoke()
    output_data = interpreter.get_tensor(output_details[0]['index'])
    results = np.squeeze(output_data)
    top_k = results.argsort()[-5:][::-1]
    labels = load_labels('/home/imagenet_labels.txt')
    for i in top_k:
        if floating_model:
            print('{:08.6f}: {}'.format(float(results[i]), labels[i]))
        else:
            print('{:08.6f}: {}'.format(float(results[i] / 255.0), labels[i]))
