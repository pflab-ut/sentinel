import tarfile
import os
import sys

import docker


def prelude():
    code = os.system('cargo +nightly b')
    if code != 0:
        print('cargo +nightly build failed', file=sys.stderr)
        return False
    return True


def create_new_image(client, image_name, base_image, src, dst):
    container_name = image_name
    container = client.containers.create(base_image, name=container_name)
    tar_name = 'test.tar'
    with tarfile.open(tar_name, 'w') as tar:
        tar.add(src)

    with open(tar_name, 'rb') as f:
        ok = container.put_archive(path='/', data=f)
        if not ok:
            container.remove()
            os.remove(tar_name)
            print('put_archive failed', file=sys.stderr)
            return False

    container.commit(image_name)
    container.remove()
    os.remove(tar_name)
    return True


def test_stdout(client, image_name, command, test_name):
    print(f'\nTesting {test_name} on image {image_name}')

    try:
        sentinel_stdout = client.containers.run(
            image_name, command, auto_remove=True, runtime='sentinel-debug')
    except docker.errors.ContainerError:
        print(
            f'sentinel failed to run {image_name} with command {command}',
            file=sys.stderr)
        return False

    try:
        runc_stdout = client.containers.run(
            image_name, command, auto_remove=True)
    except docker.errors.ContainerError:
        print(
            f'runc failed to run {image_name} with command {command}',
            file=sys.stderr)
        return False

    if sentinel_stdout == runc_stdout:
        print('\t\033[92m\033[1mOK\033[00m', test_name)
        return True
    else:
        print('sentinel and runc have different output', file=sys.stderr)
        return False


def test_simple_binaries(client, bin_name, command):
    def teardown(image_name, bin_file):
        if image_name:
            client.images.remove(image_name, force=True)
        os.remove(bin_file)

    target_c = f'./tests/app/{bin_name}.c'
    os.system(f'gcc -o {bin_name} {target_c}')
    image_name = f'ubuntu-{bin_name}'

    if not create_new_image(
        client,
        image_name,
        'ubuntu',
        bin_name,
        f'/{bin_name}',
    ):
        teardown(None, bin_name)
        exit(1)

    if not test_stdout(client, image_name, command, image_name):
        teardown(image_name, bin_name)
        exit(1)

    teardown(image_name, bin_name)


class Language:
    def __init__(self, name, ext):
        self.name = name
        self.ext = ext


def test_interpreter_programs(client, prog_name, lang):
    prog = f'/root/{prog_name}.{lang.ext}'

    if not test_stdout(
        client,
        f'sentinel-{lang.name}-test:debug',
        f'{lang.name} {prog}',
        f'{lang.name} {prog}',
    ):
        exit(1)


if __name__ == '__main__':
    client = docker.from_env()
    if not prelude():
        exit(1)

    if not test_stdout(client, 'hello-world', '/hello', 'hello-world'):
        exit(1)

    test_simple_binaries(client, 'hello_world', '/hello_world')
    test_simple_binaries(client, 'echo', '/echo And in the end, \
        the love you take is equal to the love you make')
    # test_simple_binaries(client, 'open', '')

    python = Language('python', 'py')
    ruby = Language('ruby', 'rb')

    test_interpreter_programs(client, 'hello_world', python)
    test_interpreter_programs(client, 'gen_thumbnail_from_url', python)
    test_interpreter_programs(client, 'cv2_decode_image', python)
    test_interpreter_programs(client, 'edge_detection', python)
    test_interpreter_programs(client, 'mobilenet_tflite', python)
    test_interpreter_programs(client, 'hello', ruby)
