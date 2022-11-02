import requests
from io import BytesIO
from PIL import Image

URL = "https://i.imgur.com/ExdKOOz.png"


if __name__ == '__main__':
    response = requests.get(URL)
    print('downloaded')
    content = BytesIO(response.content)
    img = Image.open(content)
    print('size is', img.size)
